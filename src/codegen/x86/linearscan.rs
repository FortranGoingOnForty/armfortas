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
use super::mir::{
    OpSize, X86Function, X86Inst, X86Opcode, X86Operand, X86Reg, X86RegClass, X86VReg,
};
use super::regalloc::{addr_operand_position, load, store, xmm_width_override};
use crate::codegen::shared::VRegId;
use std::collections::{HashMap, HashSet};

/// GP / FP spill-reload scratch — the naive allocator's set, kept out
/// of the allocation pool so a reload never clobbers an allocated value.
const GP_SCRATCH: [X86Reg; 2] = [X86Reg::R10, X86Reg::R11];
const FP_SCRATCH: [X86Reg; 2] = [X86Reg::Xmm14, X86Reg::Xmm15];

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

/// How a vreg occurrence resolves after allocation.
enum Resolved {
    /// Lives in a physical register — use it directly, no spill traffic.
    Reg(X86Reg),
    /// Spilled to a frame slot — load/store through scratch.
    Slot(i32),
}

/// Apply the allocation: rewrite vreg operands to physical registers
/// (assigned) or scratch load/store sequences (spilled), lay out the
/// frame, and bracket the body with callee-save save/restore. Mirrors
/// the naive allocator's per-instruction rewrite (reusing its
/// `load`/`store`/`addr_operand_position`/`xmm_width_override`
/// helpers) but only spilled vregs go through scratch — assigned vregs
/// become physical registers in place. After this pass no `VReg` or
/// `FrameSlot` operands remain and `frame_bytes` is final.
pub fn apply_allocation(f: &mut X86Function, result: &AllocResult) {
    // Slot per callee-saved register (saved/restored via the frame, not
    // push/pop, so emit's rbp-relative epilogue is untouched).
    let mut callee_slot: Vec<(X86Reg, i32)> = Vec::new();
    for &reg in &result.callee_saved_used {
        callee_slot.push((reg, f.alloc_frame_slot(8, 8)));
    }

    // Per-vreg class, for spill load/store sizing.
    let mut vreg_class: HashMap<VRegId, X86RegClass> = HashMap::new();
    for block in &f.blocks {
        for inst in &block.insts {
            for op in inst.operands.iter().chain(inst.def.iter()) {
                if let X86Operand::VReg(v) = op {
                    vreg_class.insert(v.id, v.class);
                }
            }
        }
    }

    // Frame layout: slot id → negative rbp displacement (identical to
    // regalloc_naive Phase 2).
    let mut offset: i64 = 0;
    let mut slot_disp: HashMap<i32, i64> = HashMap::new();
    for slot in &f.frame_slots {
        let align = slot.align.max(1) as i64;
        offset = -(((-offset) + slot.size as i64 + align - 1) & !(align - 1));
        slot_disp.insert(slot.id, offset);
    }
    let locals = -offset;
    f.frame_bytes = (locals + f.outgoing_arg_bytes + 15) & !15;

    let mem_for_slot = |slot: i32| -> X86Operand {
        X86Operand::Mem {
            base: Some(X86Reg::Rbp),
            index: None,
            scale: 1,
            disp: *slot_disp
                .get(&slot)
                .unwrap_or_else(|| panic!("frame slot {} has no layout", slot)),
        }
    };
    let resolve = |v: &X86VReg| -> Resolved {
        if let Some(&phys) = result.assignments.get(&v.id) {
            Resolved::Reg(phys)
        } else if let Some(&slot) = result.spills.get(&v.id) {
            Resolved::Slot(slot)
        } else {
            // Every vreg is either assigned or spilled.
            panic!("vreg {:?} neither assigned nor spilled", v.id);
        }
    };

    for block_idx in 0..f.blocks.len() {
        let insts = std::mem::take(&mut f.blocks[block_idx].insts);
        let mut out = Vec::with_capacity(insts.len() * 2);
        for mut inst in insts {
            let mut gp_used = 0usize;
            let mut fp_used = 0usize;
            let next_scratch = |class: X86RegClass, gp: &mut usize, fp: &mut usize| -> X86Reg {
                match class {
                    X86RegClass::Xmm | X86RegClass::Xmm128 => {
                        let r = FP_SCRATCH[(*fp).min(1)];
                        *fp += 1;
                        r
                    }
                    _ => {
                        let r = GP_SCRATCH[(*gp).min(1)];
                        *gp += 1;
                        r
                    }
                }
            };

            let tied = inst.opcode.tied_use().is_some()
                && matches!((&inst.def, inst.operands.first()),
                    (Some(X86Operand::VReg(d)), Some(X86Operand::VReg(a))) if d.id == a.id);

            let mut def_store: Option<(X86Reg, X86RegClass, i32)> = None;

            if tied {
                let v = match inst.operands[0] {
                    X86Operand::VReg(v) => v,
                    _ => unreachable!(),
                };
                match resolve(&v) {
                    Resolved::Reg(phys) => {
                        inst.operands[0] = X86Operand::Reg(phys);
                        inst.def = Some(X86Operand::Reg(phys));
                    }
                    Resolved::Slot(slot) => {
                        let scratch = next_scratch(v.class, &mut gp_used, &mut fp_used);
                        out.push(load(scratch, v.class, mem_for_slot(slot), inst.size));
                        inst.operands[0] = X86Operand::Reg(scratch);
                        inst.def = Some(X86Operand::Reg(scratch));
                        def_store = Some((scratch, v.class, slot));
                    }
                }
            }

            let addr_position = addr_operand_position(&inst);
            let (xmm_use_width, xmm_def_width) = xmm_width_override(inst.opcode);
            for (i, op) in inst.operands.iter_mut().enumerate() {
                if tied && i == 0 {
                    continue;
                }
                match op {
                    X86Operand::VReg(v) => {
                        let is_addr = Some(i) == addr_position;
                        match resolve(v) {
                            Resolved::Reg(phys) => {
                                *op = if is_addr {
                                    X86Operand::Mem {
                                        base: Some(phys),
                                        index: None,
                                        scale: 1,
                                        disp: 0,
                                    }
                                } else {
                                    X86Operand::Reg(phys)
                                };
                            }
                            Resolved::Slot(slot) => {
                                let scratch = next_scratch(v.class, &mut gp_used, &mut fp_used);
                                let load_size = if is_addr {
                                    OpSize::Q
                                } else if v.class == X86RegClass::Xmm {
                                    xmm_use_width.unwrap_or(inst.size)
                                } else {
                                    inst.size
                                };
                                out.push(load(scratch, v.class, mem_for_slot(slot), load_size));
                                *op = if is_addr {
                                    X86Operand::Mem {
                                        base: Some(scratch),
                                        index: None,
                                        scale: 1,
                                        disp: 0,
                                    }
                                } else {
                                    X86Operand::Reg(scratch)
                                };
                            }
                        }
                    }
                    X86Operand::FrameSlot(slot) => {
                        *op = mem_for_slot(*slot);
                    }
                    _ => {}
                }
            }

            if !tied {
                if let Some(X86Operand::VReg(v)) = inst.def.clone() {
                    let is_fp_store_addr = matches!(
                        inst.opcode,
                        X86Opcode::Movss | X86Opcode::Movsd | X86Opcode::Movups
                    ) && v.class != X86RegClass::Xmm
                        && v.class != X86RegClass::Xmm128;
                    match resolve(&v) {
                        Resolved::Reg(phys) => {
                            inst.def = Some(if is_fp_store_addr {
                                X86Operand::Mem {
                                    base: Some(phys),
                                    index: None,
                                    scale: 1,
                                    disp: 0,
                                }
                            } else {
                                X86Operand::Reg(phys)
                            });
                        }
                        Resolved::Slot(slot) => {
                            let scratch = next_scratch(v.class, &mut gp_used, &mut fp_used);
                            if is_fp_store_addr {
                                out.push(load(scratch, v.class, mem_for_slot(slot), OpSize::Q));
                                inst.def = Some(X86Operand::Mem {
                                    base: Some(scratch),
                                    index: None,
                                    scale: 1,
                                    disp: 0,
                                });
                            } else {
                                inst.def = Some(X86Operand::Reg(scratch));
                                def_store = Some((scratch, v.class, slot));
                            }
                        }
                    }
                } else if let Some(X86Operand::FrameSlot(slot)) = inst.def.clone() {
                    inst.def = Some(mem_for_slot(slot));
                }
            }

            let size = inst.size;
            out.push(inst);
            if let Some((scratch, class, slot)) = def_store {
                let store_size = if class == X86RegClass::Xmm {
                    xmm_def_width.unwrap_or(size)
                } else if size == OpSize::L {
                    // 32-bit ops zero-extend through bit 63 — store the
                    // full quad so an i64 consumer sees a clean value
                    // (X64-O1-002).
                    OpSize::Q
                } else {
                    size
                };
                out.push(store(scratch, class, mem_for_slot(slot), store_size));
            }
        }
        f.blocks[block_idx].insts = out;
    }

    // Callee-save save (block-0 prologue) and restore (before each Ret).
    if !callee_slot.is_empty() {
        let saves: Vec<X86Inst> = callee_slot
            .iter()
            .map(|&(reg, slot)| store(reg, X86RegClass::Gp64, mem_for_slot(slot), OpSize::Q))
            .collect();
        let restores: Vec<X86Inst> = callee_slot
            .iter()
            .map(|&(reg, slot)| load(reg, X86RegClass::Gp64, mem_for_slot(slot), OpSize::Q))
            .collect();
        // Restores before every Ret.
        for block in &mut f.blocks {
            let mut i = 0;
            while i < block.insts.len() {
                if matches!(block.insts[i].opcode, X86Opcode::Ret) {
                    for (k, r) in restores.iter().enumerate() {
                        block.insts.insert(i + k, r.clone());
                    }
                    i += restores.len() + 1;
                } else {
                    i += 1;
                }
            }
        }
        // Saves at the very start of block 0 (runs after emit's frame
        // setup, which establishes rbp before any block-0 instruction).
        let head = &mut f.blocks[0].insts;
        for (k, s) in saves.into_iter().enumerate() {
            head.insert(k, s);
        }
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
