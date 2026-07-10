//! Optimization-level → pass pipeline mapping.
//!
//! `OptLevel` is what the driver hands us; `build_pipeline` returns a
//! configured `PassManager`. Adding a new pass to a level is a one-line
//! change here, which keeps the dispatch logic in one place.

use super::bce::Bce;
use super::call_resolve::CallResolve;
use super::const_arg::ConstArgSpecialize;
use super::const_fold::ConstFold;
use super::const_prop::ConstProp;
use super::cse::LocalCse;
use super::dce::Dce;
use super::dead_arg::DeadArgElim;
use super::dead_func::DeadFuncElim;
use super::dse::Dse;
use super::fast_math::FastMathReassoc;
use super::fission::LoopFission;
use super::fusion::LoopFusion;
use super::global_lsf::GlobalLsf;
use super::gvn::Gvn;
use super::inline::Inline;
use super::interchange::LoopInterchange;
use super::jump_thread::JumpThread;
use super::licm::Licm;
use super::lsf::LocalLsf;
use super::mem2reg::Mem2Reg;
use super::neon_vectorize::{NeonVectorize, SseVectorize};
use super::pass::PassManager;
use super::peel::LoopPeel;
use super::preheader::PreheaderInsert;
use super::return_prop::ReturnPropagate;
use super::sccp::Sccp_;
use super::simplify_cfg::SimplifyCfg;
use super::sroa::Sroa;
use super::strength_reduce::StrengthReduce;
use super::unroll::LoopUnroll;
use super::unswitch::LoopUnswitch;
use super::vectorize::Vectorize;

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
        matches!(
            self,
            Self::O1 | Self::O2 | Self::O3 | Self::Os | Self::Ofast
        )
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

/// Build the pass pipeline for a given optimization level and target
/// architecture.
///
/// Adding a new optimization pass is a single push here. Keeping this
/// in one function makes it trivial to audit which passes run at which
/// level.
///
/// The pipeline is target-neutral except the loop vectorizer, which
/// registers a per-arch driver at O3/Ofast: `NeonVectorize` on arm64,
/// `SseVectorize` on x86_64 (x10) — same analysis and rewrites, a
/// per-ISA legality table. `Vectorize` (runtime bulk kernels with
/// scalar fallbacks) registers on both.
pub fn build_pipeline(level: OptLevel, arch: crate::target::Arch) -> PassManager {
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
            pm.add(Box::new(CallResolve));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(Inline::for_level(OptLevel::O1)));
            pm.add(Box::new(ConstArgSpecialize));
            pm.add(Box::new(DeadArgElim));
            pm.add(Box::new(ReturnPropagate));
            pm.add(Box::new(SimplifyCfg));
            pm.add(Box::new(DeadFuncElim));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(Sccp_));
            pm.add(Box::new(JumpThread));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dce));
        }
        OptLevel::O2 => {
            // O1 plus LICM, strength reduction, DSE, LSF, loop transforms.
            pm.add(Box::new(CallResolve));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(Sroa)); // after SSA + const fold (GCC pattern)
            pm.add(Box::new(Mem2Reg)); // re-promote SROA-created scalar allocas
            pm.add(Box::new(Inline::for_level(OptLevel::O2)));
            pm.add(Box::new(ConstArgSpecialize));
            pm.add(Box::new(DeadArgElim));
            pm.add(Box::new(ReturnPropagate));
            pm.add(Box::new(SimplifyCfg));
            pm.add(Box::new(DeadFuncElim));
            pm.add(Box::new(Bce));
            pm.add(Box::new(StrengthReduce));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(GlobalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(PreheaderInsert));
            pm.add(Box::new(LoopPeel));
            pm.add(Box::new(LoopUnswitch));
            pm.add(Box::new(Licm));
            pm.add(Box::new(Sccp_));
            pm.add(Box::new(JumpThread));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dse));
            pm.add(Box::new(LoopInterchange));
            pm.add(Box::new(LoopFission));
            pm.add(Box::new(LoopFusion));
            pm.add(Box::new(LoopUnroll));
            pm.add(Box::new(Gvn)); // after loop passes to avoid SSA conflicts
            pm.add(Box::new(Dce));
        }
        OptLevel::Os => {
            // Like O2 but no loop unrolling (prefer code size).
            pm.add(Box::new(CallResolve));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(Sroa));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(Inline::for_level(OptLevel::Os)));
            pm.add(Box::new(ConstArgSpecialize));
            pm.add(Box::new(DeadArgElim));
            pm.add(Box::new(ReturnPropagate));
            pm.add(Box::new(SimplifyCfg));
            pm.add(Box::new(DeadFuncElim));
            pm.add(Box::new(Bce));
            pm.add(Box::new(StrengthReduce));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(GlobalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(PreheaderInsert));
            pm.add(Box::new(LoopPeel));
            pm.add(Box::new(LoopUnswitch));
            pm.add(Box::new(Licm));
            pm.add(Box::new(Sccp_));
            pm.add(Box::new(JumpThread));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dse));
            pm.add(Box::new(LoopInterchange));
            pm.add(Box::new(Gvn));
            pm.add(Box::new(Dce));
        }
        OptLevel::O3 => {
            // O2 passes + loop unrolling + interchange.
            pm.add(Box::new(CallResolve));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(Sroa));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(Inline::for_level(OptLevel::O3)));
            pm.add(Box::new(ConstArgSpecialize));
            pm.add(Box::new(DeadArgElim));
            pm.add(Box::new(ReturnPropagate));
            pm.add(Box::new(SimplifyCfg));
            pm.add(Box::new(DeadFuncElim));
            pm.add(Box::new(Bce));
            pm.add(Box::new(StrengthReduce));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(GlobalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(PreheaderInsert));
            pm.add(Box::new(LoopPeel));
            pm.add(Box::new(LoopUnswitch));
            pm.add(Box::new(Licm));
            pm.add(Box::new(Sccp_));
            pm.add(Box::new(JumpThread));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dse));
            pm.add(Box::new(LoopInterchange));
            pm.add(Box::new(LoopFission));
            pm.add(Box::new(LoopFusion));
            match arch {
                crate::target::Arch::Arm64 => {
                    pm.add(Box::new(NeonVectorize));
                    pm.add(Box::new(Vectorize));
                }
                crate::target::Arch::X86_64 => {
                    pm.add(Box::new(SseVectorize));
                    pm.add(Box::new(Vectorize));
                }
            }
            pm.add(Box::new(LoopUnroll));
            pm.add(Box::new(Gvn)); // keep O3/Ofast aligned with O2/Os value numbering
            pm.add(Box::new(Dce));
        }
        OptLevel::Ofast => {
            // O3 plus Ofast-only fast-math reassociation.
            pm.add(Box::new(CallResolve));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(ConstFold));
            pm.add(Box::new(Sroa));
            pm.add(Box::new(Mem2Reg));
            pm.add(Box::new(Inline::for_level(OptLevel::O3)));
            pm.add(Box::new(ConstArgSpecialize));
            pm.add(Box::new(DeadArgElim));
            pm.add(Box::new(ReturnPropagate));
            pm.add(Box::new(SimplifyCfg));
            pm.add(Box::new(DeadFuncElim));
            pm.add(Box::new(Bce));
            pm.add(Box::new(StrengthReduce));
            pm.add(Box::new(LocalLsf));
            pm.add(Box::new(GlobalLsf));
            pm.add(Box::new(LocalCse));
            pm.add(Box::new(PreheaderInsert));
            pm.add(Box::new(LoopPeel));
            pm.add(Box::new(LoopUnswitch));
            pm.add(Box::new(Licm));
            pm.add(Box::new(Sccp_));
            pm.add(Box::new(JumpThread));
            pm.add(Box::new(ConstProp));
            pm.add(Box::new(Dse));
            pm.add(Box::new(LoopInterchange));
            pm.add(Box::new(LoopFission));
            pm.add(Box::new(LoopFusion));
            match arch {
                crate::target::Arch::Arm64 => {
                    pm.add(Box::new(NeonVectorize));
                    pm.add(Box::new(Vectorize));
                }
                crate::target::Arch::X86_64 => {
                    pm.add(Box::new(SseVectorize));
                    pm.add(Box::new(Vectorize));
                }
            }
            pm.add(Box::new(LoopUnroll));
            pm.add(Box::new(FastMathReassoc));
            pm.add(Box::new(Gvn));
            pm.add(Box::new(Dce));
        }
    }
    pm
}

/// Build the restricted optimization pipeline for modules that still contain
/// non-global `i128` values.
///
/// This deliberately widens `i128` support one optimization lane at a time.
/// Now that the backend can carry stack-backed `i128` values through block
/// params and mem2reg-style joins, the widened `i128` lane can use the full
/// ordinary O1/O2/O3/Os/Ofast pipelines. Higher levels remain gated until their
/// pass shapes are proven end to end.
pub fn build_i128_pipeline(level: OptLevel, arch: crate::target::Arch) -> Option<PassManager> {
    match level {
        OptLevel::O0 => None,
        _ => Some(build_pipeline(level, arch)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Arch;

    #[test]
    fn parse_flags() {
        assert_eq!(OptLevel::parse_flag("O0"), Some(OptLevel::O0));
        assert_eq!(OptLevel::parse_flag("Os"), Some(OptLevel::Os));
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
        assert!(build_pipeline(OptLevel::O0, Arch::Arm64).is_empty());
        for lvl in [
            OptLevel::O1,
            OptLevel::O2,
            OptLevel::O3,
            OptLevel::Os,
            OptLevel::Ofast,
        ] {
            let pm = build_pipeline(lvl, Arch::Arm64);
            assert!(
                !pm.is_empty(),
                "pipeline {:?} should have at least one pass",
                lvl
            );
        }
    }

    #[test]
    fn higher_optimization_levels_keep_gvn_enabled() {
        for lvl in [OptLevel::O2, OptLevel::O3, OptLevel::Os, OptLevel::Ofast] {
            let pm = build_pipeline(lvl, Arch::Arm64);
            let names = pm.pass_names();
            assert!(
                names.contains(&"gvn"),
                "pipeline {:?} should include gvn, got {:?}",
                lvl,
                names
            );
        }
    }

    #[test]
    fn ofast_enables_fast_math_reassoc_but_o3_does_not() {
        let o3 = build_pipeline(OptLevel::O3, Arch::Arm64).pass_names();
        let ofast = build_pipeline(OptLevel::Ofast, Arch::Arm64).pass_names();
        assert!(
            !o3.contains(&"fast-math-reassoc"),
            "O3 should stay strict, got {:?}",
            o3
        );
        assert!(
            ofast.contains(&"fast-math-reassoc"),
            "Ofast should include fast-math reassociation, got {:?}",
            ofast
        );
    }

    #[test]
    fn vectorize_is_enabled_only_at_o3_and_above() {
        let o2 = build_pipeline(OptLevel::O2, Arch::Arm64).pass_names();
        let o3 = build_pipeline(OptLevel::O3, Arch::Arm64).pass_names();
        let ofast = build_pipeline(OptLevel::Ofast, Arch::Arm64).pass_names();

        assert!(
            !o2.contains(&"vectorize"),
            "O2 should not include vectorize, got {:?}",
            o2
        );
        assert!(
            o3.contains(&"vectorize"),
            "O3 should include vectorize, got {:?}",
            o3
        );
        assert!(
            ofast.contains(&"vectorize"),
            "Ofast should include vectorize, got {:?}",
            ofast
        );
    }

    #[test]
    fn x86_pipeline_matches_arm_with_sse_vectorizer() {
        // x10: the only per-target pipeline difference is which loop
        // vectorizer driver registers at O3/Ofast. Everything else
        // must stay identical across arches.
        for lvl in [
            OptLevel::O0,
            OptLevel::O1,
            OptLevel::O2,
            OptLevel::O3,
            OptLevel::Os,
            OptLevel::Ofast,
        ] {
            let arm: Vec<_> = build_pipeline(lvl, Arch::Arm64)
                .pass_names()
                .into_iter()
                .map(|n| {
                    if n == "neon_vectorize" {
                        "sse2_vectorize"
                    } else {
                        n
                    }
                })
                .collect();
            let x86 = build_pipeline(lvl, Arch::X86_64).pass_names();
            assert_eq!(
                arm, x86,
                "{:?}: x86 pipeline should be the arm64 pipeline with the SSE driver",
                lvl
            );
            assert!(
                !x86.contains(&"neon_vectorize"),
                "{:?}: the NEON driver must not register for x86_64, got {:?}",
                lvl,
                x86
            );
        }
    }

    #[test]
    fn arm64_o3_keeps_vectorizers() {
        // Golden guard: the gating change must not alter the arm64
        // registered pass list.
        let o3 = build_pipeline(OptLevel::O3, Arch::Arm64).pass_names();
        assert!(o3.contains(&"neon_vectorize"), "got {:?}", o3);
        assert!(o3.contains(&"vectorize"), "got {:?}", o3);
    }

    #[test]
    fn i128_pipeline_is_available_through_ofast() {
        assert!(
            build_i128_pipeline(OptLevel::O1, Arch::Arm64).is_some(),
            "O1 should have the widened i128-safe pipeline"
        );
        assert!(
            build_i128_pipeline(OptLevel::O2, Arch::Arm64).is_some(),
            "O2 should be available once the widened i128 lane is proven"
        );
        for lvl in [OptLevel::O3, OptLevel::Os, OptLevel::Ofast] {
            assert!(
                build_i128_pipeline(lvl, Arch::Arm64).is_some(),
                "{:?} should be available once the widened i128 lane is proven",
                lvl
            );
        }
        let lvl = OptLevel::O0;
        assert!(
            build_i128_pipeline(lvl, Arch::Arm64).is_none(),
            "{:?} should not yet have widened i128 optimization support",
            lvl
        );
    }

    #[test]
    fn i128_pipeline_matches_full_o1() {
        let wide = build_i128_pipeline(OptLevel::O1, Arch::Arm64)
            .expect("O1 should expose the widened i128 pipeline")
            .pass_names();
        let full = build_pipeline(OptLevel::O1, Arch::Arm64).pass_names();
        assert_eq!(
            wide, full,
            "the widened i128 O1 lane should stay aligned with the ordinary O1 pipeline"
        );
    }

    #[test]
    fn i128_pipeline_matches_full_higher_levels() {
        for lvl in [OptLevel::O2, OptLevel::O3, OptLevel::Os, OptLevel::Ofast] {
            let wide = build_i128_pipeline(lvl, Arch::Arm64)
                .expect("level should expose the widened i128 pipeline")
                .pass_names();
            let full = build_pipeline(lvl, Arch::Arm64).pass_names();
            assert_eq!(
                wide, full,
                "the widened i128 lane should stay aligned with the ordinary {:?} pipeline",
                lvl
            );
        }
    }
}
