//! Linear-scan register allocator for the x86_64 backend (x10a).
//!
//! Forked from `arm64::linearscan` (the MirView-share survey found the
//! arm64 allocator too arm64-coupled to generalize cleanly — see the
//! x10a sprint doc). This is the first correct version: it assigns
//! registers and falls back to spilling, WITHOUT arm64's live-range
//! splitter (that is a later optimization, not correctness).
//!
//! ## Register pools — why so small (for now)
//!
//! x86 isel hard-codes physical registers all over: rax/rdx for idiv,
//! imul-high, i128 pairs and returns; rcx for shift counts; rdi/rsi/
//! rdx/rcx/r8/r9 and xmm0-7 for the SysV call sequence. Assigning a
//! vreg to any of those risks a collision with an isel-placed `Reg`
//! operand that overlaps the vreg's live range. Modelling those as
//! fixed intervals is a later iteration; this version allocates ONLY
//! registers isel never hard-codes:
//!
//!   GP: rbx, r12, r13, r14, r15  (callee-saved — survive calls)
//!   FP: xmm8..xmm13              (caller-saved — clobbered by calls)
//!
//! r10/r11 and xmm14/xmm15 stay reserved as spill-reload scratch (the
//! naive allocator's scratch set), kept OUT of the pool so a reload
//! never clobbers an allocated value. GP allocations all land in
//! callee-saved registers, so they survive calls automatically; FP
//! call-crossing intervals have no safe callee-saved register on SysV
//! and are spilled.

use super::liveness::{compute_liveness, LiveInterval, LivenessResult};
use super::mir::{X86Function, X86Reg, X86RegClass};
use crate::codegen::shared::VRegId;
use std::collections::{HashMap, HashSet};

/// GP registers available for allocation: callee-saved only, none of
/// which isel ever hard-codes.
const GP_ALLOC_ORDER: [X86Reg; 5] = [
    X86Reg::Rbx,
    X86Reg::R12,
    X86Reg::R13,
    X86Reg::R14,
    X86Reg::R15,
];

/// XMM registers available for allocation: xmm8..xmm13 carry no fixed
/// ABI role (xmm0-7 are arg/return, xmm14/xmm15 are spill scratch).
const FP_ALLOC_ORDER: [X86Reg; 6] = [
    X86Reg::Xmm8,
    X86Reg::Xmm9,
    X86Reg::Xmm10,
    X86Reg::Xmm11,
    X86Reg::Xmm12,
    X86Reg::Xmm13,
];

fn is_callee_saved(reg: X86Reg) -> bool {
    matches!(
        reg,
        X86Reg::Rbx | X86Reg::R12 | X86Reg::R13 | X86Reg::R14 | X86Reg::R15
    )
}

fn is_fp_class(class: X86RegClass) -> bool {
    matches!(class, X86RegClass::Xmm | X86RegClass::Xmm128)
}

/// Bytes one spill of a vreg of `class` occupies. Xmm128 needs the full
/// 16; everything else (including scalar Xmm — stored via movsd) fits 8.
fn spill_slot_size(class: X86RegClass) -> (u64, u64) {
    match class {
        X86RegClass::Xmm128 => (16, 16),
        _ => (8, 8),
    }
}

/// Result of register allocation.
pub struct AllocResult {
    /// VRegId → assigned physical register.
    pub assignments: HashMap<VRegId, X86Reg>,
    /// Spilled vregs → frame slot id (resolved to an rbp displacement
    /// by frame layout).
    pub spills: HashMap<VRegId, i32>,
    /// Callee-saved registers used, needing prologue save / epilogue
    /// restore. Sorted for deterministic save order.
    pub callee_saved_used: Vec<X86Reg>,
    /// The liveness result the assignment was built from (apply pass
    /// reuses the intervals/positions).
    pub liveness: LivenessResult,
}

fn expire_intervals(active: &mut Vec<(X86Reg, u32, VRegId)>, free: &mut Vec<X86Reg>, pos: u32) {
    let mut i = 0;
    while i < active.len() {
        if active[i].1 < pos {
            let (reg, _, _) = active.remove(i);
            free.push(reg);
        } else {
            i += 1;
        }
    }
}

/// Run linear-scan allocation. Allocates spill frame slots into `f`
/// (frame layout runs later, in the apply pass).
pub fn linear_scan(f: &mut X86Function) -> AllocResult {
    let liveness = compute_liveness(f);

    let mut assignments: HashMap<VRegId, X86Reg> = HashMap::new();
    let mut spills: HashMap<VRegId, i32> = HashMap::new();
    let mut callee_saved_used: HashSet<X86Reg> = HashSet::new();

    // Per-class active lists (reg, interval_end, vreg), kept sorted by
    // end so the spill victim is always the last entry.
    let mut active_gp: Vec<(X86Reg, u32, VRegId)> = Vec::new();
    let mut active_fp: Vec<(X86Reg, u32, VRegId)> = Vec::new();
    // Free pools, consumed in listed order (remove(0)).
    let mut free_gp: Vec<X86Reg> = GP_ALLOC_ORDER.to_vec();
    let mut free_fp: Vec<X86Reg> = FP_ALLOC_ORDER.to_vec();

    // Per-vreg class for spill-slot sizing of victims.
    let vreg_class: HashMap<VRegId, X86RegClass> = liveness
        .intervals
        .iter()
        .map(|i| (i.vreg, i.class))
        .collect();

    let intervals: Vec<LiveInterval> = liveness.intervals.clone();
    for interval in &intervals {
        let is_fp = is_fp_class(interval.class);
        let (active, free) = if is_fp {
            (&mut active_fp, &mut free_fp)
        } else {
            (&mut active_gp, &mut free_gp)
        };
        expire_intervals(active, free, interval.start);

        // FP call-crossing intervals: the FP pool is caller-saved
        // (clobbered by the call), and SysV has no callee-saved xmm, so
        // spill rather than allocate. GP allocations are all
        // callee-saved and survive calls, so no special case there.
        if is_fp && interval.crosses_call {
            let (sz, al) = spill_slot_size(interval.class);
            let slot = f.alloc_frame_slot(sz, al);
            spills.insert(interval.vreg, slot);
            continue;
        }

        let reg_opt = if !free.is_empty() {
            Some(free.remove(0))
        } else {
            None
        };

        if let Some(reg) = reg_opt {
            assignments.insert(interval.vreg, reg);
            active.push((reg, interval.end, interval.vreg));
            active.sort_by_key(|&(_, end, _)| end);
            if is_callee_saved(reg) {
                callee_saved_used.insert(reg);
            }
        } else if let Some(&(spill_reg, spill_end, victim)) = active.last() {
            // No register free — spill the interval ending furthest.
            let last_idx = active.len() - 1;
            if spill_end > interval.end {
                // Evict the victim, take its register.
                let vclass = vreg_class
                    .get(&victim)
                    .copied()
                    .unwrap_or(X86RegClass::Gp64);
                let (sz, al) = spill_slot_size(vclass);
                let slot = f.alloc_frame_slot(sz, al);
                spills.insert(victim, slot);
                assignments.remove(&victim);
                assignments.insert(interval.vreg, spill_reg);
                active.remove(last_idx);
                active.push((spill_reg, interval.end, interval.vreg));
                active.sort_by_key(|&(_, end, _)| end);
                if is_callee_saved(spill_reg) {
                    callee_saved_used.insert(spill_reg);
                }
            } else {
                let (sz, al) = spill_slot_size(interval.class);
                let slot = f.alloc_frame_slot(sz, al);
                spills.insert(interval.vreg, slot);
            }
        } else {
            let (sz, al) = spill_slot_size(interval.class);
            let slot = f.alloc_frame_slot(sz, al);
            spills.insert(interval.vreg, slot);
        }
    }

    let mut callee_saved: Vec<X86Reg> = callee_saved_used.into_iter().collect();
    callee_saved.sort_by_key(reg_sort_key);

    AllocResult {
        assignments,
        spills,
        callee_saved_used: callee_saved,
        liveness,
    }
}

/// Stable ordering key for deterministic callee-save sequences.
fn reg_sort_key(reg: &X86Reg) -> u8 {
    match reg {
        X86Reg::Rbx => 0,
        X86Reg::R12 => 1,
        X86Reg::R13 => 2,
        X86Reg::R14 => 3,
        X86Reg::R15 => 4,
        _ => 255,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::x86::isel::select_function;
    use crate::ir::builder::FuncBuilder;
    use crate::ir::inst::*;
    use crate::ir::types::*;

    #[test]
    fn assigns_registers() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mut mf = select_function(&func, &[], crate::target::TargetLayout::LP64);
        let result = linear_scan(&mut mf);
        // Every assigned register is from the allocatable pool, never a
        // scratch (r10/r11/xmm14/xmm15) or a fixed-role register.
        for &reg in result.assignments.values() {
            assert!(
                GP_ALLOC_ORDER.contains(&reg) || FP_ALLOC_ORDER.contains(&reg),
                "assigned a non-pool register: {:?}",
                reg
            );
        }
    }

    #[test]
    fn callee_saved_tracked() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let x = b.const_i32(1);
            let y = b.const_i32(2);
            let z = b.iadd(x, y);
            let w = b.imul(z, x);
            let _v = b.iadd(w, y);
            b.ret_void();
        }
        let mut mf = select_function(&func, &[], crate::target::TargetLayout::LP64);
        let result = linear_scan(&mut mf);
        // Any GP assignment is callee-saved, so it must be recorded.
        for &reg in result.assignments.values() {
            if is_callee_saved(reg) {
                assert!(
                    result.callee_saved_used.contains(&reg),
                    "{:?} assigned but not in callee_saved_used",
                    reg
                );
            }
        }
    }
}
