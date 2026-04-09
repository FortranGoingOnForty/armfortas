//! Optimization-level → pass pipeline mapping.
//!
//! `OptLevel` is what the driver hands us; `build_pipeline` returns a
//! configured `PassManager`. Adding a new pass to a level is a one-line
//! change here, which keeps the dispatch logic in one place.

use super::pass::PassManager;
use super::const_fold::ConstFold;
use super::const_prop::ConstProp;
use super::dce::Dce;
use super::cse::LocalCse;
use super::strength_reduce::StrengthReduce;
use super::licm::Licm;
use super::mem2reg::Mem2Reg;
use super::dse::Dse;
use super::lsf::LocalLsf;
use super::unroll::LoopUnroll;
use super::preheader::PreheaderInsert;
use super::unswitch::LoopUnswitch;
use super::interchange::LoopInterchange;
use super::peel::LoopPeel;
use super::fission::LoopFission;
use super::fusion::LoopFusion;

/// Compiler optimization levels.
///
/// Mirrors `gfortran` / `clang` semantics so users have no surprises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    /// `-O0` — no optimization. Default during development.
    O0,
    /// `-O1` — constant folding, DCE, basic CSE, copy propagation.
    O1,
    /// `-O2` — `-O1` plus LICM, small inlining, strength reduction,
    /// bounds-check elimination, GVN, SROA, dead store elim, small loop
    /// unrolling, FMA fusion.
    O2,
    /// `-O3` — `-O2` plus aggressive inlining, NEON vectorization,
    /// loop interchange/fusion/fission, IPO, devirtualization,
    /// whole-program analysis, speculative optimizations.
    O3,
    /// `-Os` — like `-O2` but prefer code size (no unrolling, less inlining).
    Os,
    /// `-Ofast` — `-O3` plus fast-math (reassociation, no NaN/Inf, recip).
    Ofast,
}

impl OptLevel {
    /// Parse the textual flag (`O0`, `O1`, ..., `Ofast`).
    pub fn parse_flag(s: &str) -> Option<Self> {
        match s {
            "O0" | "0" => Some(Self::O0),
            "O1" | "1" => Some(Self::O1),
            "O2" | "2" => Some(Self::O2),
            "O3" | "3" => Some(Self::O3),
            "Os" | "s" => Some(Self::Os),
            "Ofast" | "fast" => Some(Self::Ofast),
            _ => None,
        }
    }

    pub fn flag_name(self) -> &'static str {
        match self {
            Self::O0 => "-O0",
            Self::O1 => "-O1",
            Self::O2 => "-O2",
            Self::O3 => "-O3",
            Self::Os => "-Os",
            Self::Ofast => "-Ofast",
        }
    }

    /// Does this level enable inlining?
    ///
    /// Audit Min-6: this predicate is currently consulted only by the
    /// pipeline test harness. Once `Inline` lands as a pass, the
    /// builder below will gate registration on this. Same for the
    /// other two predicates.
    pub fn inlining(self) -> bool {
        matches!(self, Self::O2 | Self::O3 | Self::Os | Self::Ofast)
    }

    /// Does this level enable loop vectorization (NEON)?
    pub fn vectorize(self) -> bool {
        matches!(self, Self::O3 | Self::Ofast)
    }

    /// Does this level allow value-changing fast-math reassociation
    /// (`-Ofast`-only — relaxes IEEE 754 strictness for FAdd/FMul
    /// reordering, signed-zero collapse, etc.)?
    pub fn fast_math(self) -> bool {
        matches!(self, Self::Ofast)
    }
}

/// Build the pass pipeline for a given optimization level.
///
/// Adding a new optimization pass is a single push here. Keeping this
/// in one function makes it trivial to audit which passes run at which
/// level.
pub fn build_pipeline(level: OptLevel) -> PassManager {
    let mut pm = PassManager::new();
    match level {
        OptLevel::O0 => {
            // Nothing — preserve unoptimized IR exactly as it was lowered.
        }
        OptLevel::O1 => {
            // Cheap, always-correct cleanup.
            //
            // Mem2reg runs FIRST so every downstream pass sees SSA
            // values instead of alloca/load/store round-trips.
            // Without it, const_fold can't propagate constants
            // through local variables, CSE can't dedupe across
            // store/load pairs, and LICM is effectively dormant
            // (loads block every hoist attempt).
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dce));
        }
        OptLevel::O2 => {
            // O1 plus LICM, strength reduction, DSE, LSF, loop transforms.
            // LoopPeel, LoopFission, LoopFusion disabled pending 29.6.7
            // rewrite (audit: peel has 5 bugs; fission/fusion are stubs).
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(StrengthReduce));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(PreheaderInsert));
            pm.add(Box::new(LoopUnswitch));
            pm.add(Box::new(Licm));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dse));
            pm.add(Box::new(LoopInterchange));
            pm.add(Box::new(LoopUnroll));
            pm.add(Box::new(Dce));
        }
        OptLevel::Os => {
            // Like O2 but no loop unrolling (prefer code size).
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(StrengthReduce));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(PreheaderInsert));
            pm.add(Box::new(LoopUnswitch));
            pm.add(Box::new(Licm));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dse));
            pm.add(Box::new(LoopInterchange));
            pm.add(Box::new(Dce));
        }
        OptLevel::O3 | OptLevel::Ofast => {
            // O2 passes + loop unrolling + interchange.
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(StrengthReduce));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(PreheaderInsert));
            pm.add(Box::new(LoopUnswitch));
            pm.add(Box::new(Licm));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dse));
            pm.add(Box::new(LoopInterchange));
            pm.add(Box::new(LoopUnroll));
            pm.add(Box::new(Dce));
        }
    }
    pm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flags() {
        assert_eq!(OptLevel::parse_flag("O0"), Some(OptLevel::O0));
        assert_eq!(OptLevel::parse_flag("O3"), Some(OptLevel::O3));
        assert_eq!(OptLevel::parse_flag("Ofast"), Some(OptLevel::Ofast));
        assert_eq!(OptLevel::parse_flag("O9"), None);
    }

    #[test]
    fn level_predicates() {
        assert!(!OptLevel::O0.inlining());
        assert!(OptLevel::O2.inlining());
        assert!(OptLevel::O3.vectorize());
        assert!(!OptLevel::O2.vectorize());
        assert!(OptLevel::Ofast.fast_math());
        assert!(!OptLevel::O3.fast_math());
    }

    #[test]
    fn pipelines_build() {
        // O0 has no passes; every other level has at least one.
        assert!(build_pipeline(OptLevel::O0).is_empty());
        for lvl in [OptLevel::O1, OptLevel::O2, OptLevel::O3, OptLevel::Os, OptLevel::Ofast] {
            let pm = build_pipeline(lvl);
            assert!(!pm.is_empty(), "pipeline {:?} should have at least one pass", lvl);
        }
    }
}
