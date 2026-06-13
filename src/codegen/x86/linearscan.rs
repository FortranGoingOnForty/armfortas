//! Linear-scan register allocator for the x86_64 backend (x10a / x10a-1).
//!
//! Forked from `arm64::linearscan` (the MirView-share survey found the
//! arm64 allocator too arm64-coupled to generalize cleanly — see the
//! x10a sprint doc). Sort live intervals by start, walk in order,
//! assign physical registers; on pressure, evict the furthest-ending
//! active interval or spill. No live-range splitting yet (that and
//! coalescing are x10a-2).
//!
//! ## Full register file via fixed intervals (x10a-1)
//!
//! x86 isel hard-codes physical registers everywhere: rax/rdx for idiv,
//! i128 pairs and returns; rcx for shift counts; rdi/rsi/rdx/rcx/r8/r9
//! and xmm0-7 for the SysV call sequence; and every Call clobbers all
//! caller-saved registers. Rather than exclude those registers from the
//! pool, the allocatable pool is the FULL file (GP minus rsp/rbp and the
//! r10/r11 spill scratch; xmm0-13, with xmm14/15 scratch), and a
//! fixed-interval check (`build_phys_occupancy` + `phys_free_over`)
//! forbids assigning a vreg to register P over any range where isel
//! references P. `implicit_phys_effects` supplies the register effects
//! isel encodes in the opcode rather than the operand vector (Cltd/Cqto,
//! Idiv, Call clobbers) — without it a wide pool miscompiles. The check
//! subsumes call-crossing: a call occupies every caller-saved register,
//! so a crossing interval lands on a callee-saved register or spills.
//!
//! r10/r11 and xmm14/xmm15 stay reserved as spill-reload scratch (the
//! naive allocator's set), kept OUT of the pool so a reload never
//! clobbers an allocated value. The naive allocator remains the
//! correctness reference, selected at O0 and behind
//! `ARMFORTAS_USE_NAIVE_REGALLOC`.

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

/// GP registers available for allocation — the full file minus rsp/rbp
/// (frame) and r10/r11 (spill scratch). Caller-saved first (free to use
/// for intervals that don't cross a call; the fixed-interval check
/// routes call-crossing intervals away from them), callee-saved last
/// (cost a prologue save/restore). Assigning a vreg to a register isel
/// also touches is prevented by the fixed-interval check, not by
/// excluding the register here.
const GP_ALLOC_ORDER: [X86Reg; 12] = [
    // caller-saved
    X86Reg::Rax,
    X86Reg::Rcx,
    X86Reg::Rdx,
    X86Reg::Rsi,
    X86Reg::Rdi,
    X86Reg::R8,
    X86Reg::R9,
    // callee-saved
    X86Reg::Rbx,
    X86Reg::R12,
    X86Reg::R13,
    X86Reg::R14,
    X86Reg::R15,
];

/// XMM registers available for allocation — xmm0..xmm13 (xmm14/xmm15 are
/// spill scratch). All xmm are caller-saved on SysV; a call-crossing FP
/// interval has no callee-saved xmm to land in and is spilled.
const FP_ALLOC_ORDER: [X86Reg; 14] = [
    X86Reg::Xmm0,
    X86Reg::Xmm1,
    X86Reg::Xmm2,
    X86Reg::Xmm3,
    X86Reg::Xmm4,
    X86Reg::Xmm5,
    X86Reg::Xmm6,
    X86Reg::Xmm7,
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

/// Every caller-saved register a Call/CallReg clobbers (SysV: all GP
/// except rbx/rbp/r12-r15, and all xmm). Used as the implicit defs of a
/// call so the fixed-interval check keeps live values out of them across
/// the call.
const CALL_CLOBBER: [X86Reg; 25] = [
    X86Reg::Rax,
    X86Reg::Rcx,
    X86Reg::Rdx,
    X86Reg::Rsi,
    X86Reg::Rdi,
    X86Reg::R8,
    X86Reg::R9,
    X86Reg::R10,
    X86Reg::R11,
    X86Reg::Xmm0,
    X86Reg::Xmm1,
    X86Reg::Xmm2,
    X86Reg::Xmm3,
    X86Reg::Xmm4,
    X86Reg::Xmm5,
    X86Reg::Xmm6,
    X86Reg::Xmm7,
    X86Reg::Xmm8,
    X86Reg::Xmm9,
    X86Reg::Xmm10,
    X86Reg::Xmm11,
    X86Reg::Xmm12,
    X86Reg::Xmm13,
    X86Reg::Xmm14,
    X86Reg::Xmm15,
];

/// Physical registers an opcode reads/writes IMPLICITLY — encoded by
/// isel in the opcode rather than the operand vector. Returns
/// (implicit uses, implicit defs). Without these the fixed-interval
/// check would miss that Idiv clobbers rax:rdx or that a Call clobbers
/// every caller-saved register, and a wide pool would miscompile.
/// Verified against the isel physreg enumeration audit (2026-06-13).
/// Shift counts, div dividend/result moves, call-arg setup, returns,
/// and the AL varargs byte are NOT here — isel emits them as explicit
/// `Reg` operands, which the occupancy walk already sees.
fn implicit_phys_effects(opcode: X86Opcode) -> (&'static [X86Reg], &'static [X86Reg]) {
    const RAX: [X86Reg; 1] = [X86Reg::Rax];
    const RDX: [X86Reg; 1] = [X86Reg::Rdx];
    const RAX_RDX: [X86Reg; 2] = [X86Reg::Rax, X86Reg::Rdx];
    match opcode {
        X86Opcode::Cltd | X86Opcode::Cqto => (&RAX, &RDX),
        X86Opcode::Idiv => (&RAX_RDX, &RAX_RDX),
        X86Opcode::Call | X86Opcode::CallReg => (&[], &CALL_CLOBBER),
        _ => (&[], &[]),
    }
}

/// Build, per physical register, the sorted positions where isel
/// references it (explicit `Reg` in any operand or def, plus implicit
/// effects). A vreg interval [s,e] may NOT use register P if any of
/// P's occupied positions lies in [s,e] — that is the fixed-interval
/// constraint. Position numbering matches `compute_liveness` (step 2
/// per instruction, blocks in order).
fn build_phys_occupancy(f: &X86Function) -> HashMap<X86Reg, Vec<u32>> {
    let mut occ: HashMap<X86Reg, Vec<u32>> = HashMap::new();
    let mut pos: u32 = 0;
    for block in &f.blocks {
        for inst in &block.insts {
            let mut mark = |reg: X86Reg| occ.entry(reg).or_default().push(pos);
            for op in inst.operands.iter().chain(inst.def.iter()) {
                match op {
                    X86Operand::Reg(r) => mark(*r),
                    X86Operand::Mem { base, index, .. } => {
                        if let Some(b) = base {
                            mark(*b);
                        }
                        if let Some(i) = index {
                            mark(*i);
                        }
                    }
                    _ => {}
                }
            }
            let (iu, idf) = implicit_phys_effects(inst.opcode);
            for &r in iu.iter().chain(idf.iter()) {
                mark(r);
            }
            pos += 2;
        }
    }
    for positions in occ.values_mut() {
        positions.sort_unstable();
        positions.dedup();
    }
    occ
}

/// Is register P free of any isel-fixed reference across [start, end]
/// (inclusive)? Conservative-inclusive: a fixed reference exactly at the
/// vreg's def or last-use position also blocks P (safe — coalescing
/// such boundary cases is x10a-2's job).
fn phys_free_over(occ: &HashMap<X86Reg, Vec<u32>>, reg: X86Reg, start: u32, end: u32) -> bool {
    match occ.get(&reg) {
        None => true,
        Some(positions) => {
            // First position >= start; if it's <= end, P is occupied.
            let idx = positions.partition_point(|&p| p < start);
            idx >= positions.len() || positions[idx] > end
        }
    }
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

    // Fixed-interval occupancy: positions where isel references each
    // physical register. A vreg may only take a register free of any
    // such reference over its whole live range — this is what lets the
    // pool include rax/rcx/rdx/rsi/rdi/r8/r9 and xmm0-7 safely. It also
    // subsumes the call-crossing case: a call clobbers every
    // caller-saved register, so a crossing interval is forced onto a
    // callee-saved register or a spill with no special-casing.
    let occ = build_phys_occupancy(f);

    let intervals: Vec<LiveInterval> = liveness.intervals.clone();
    for interval in &intervals {
        let is_fp = is_fp_class(interval.class);
        let (active, free) = if is_fp {
            (&mut active_fp, &mut free_fp)
        } else {
            (&mut active_gp, &mut free_gp)
        };
        expire_intervals(active, free, interval.start);

        // First free register (pool preference order) with no fixed
        // reference across [start,end]. A hint, when set, jumps the
        // queue if it is free and also passes — that places the vreg in
        // the register isel moves it to/from, so coalescing drops the
        // move.
        let free_idx = interval
            .hint
            .and_then(|hr| {
                free.iter()
                    .position(|&r| r == hr)
                    .filter(|_| phys_free_over(&occ, hr, interval.start, interval.end))
            })
            .or_else(|| {
                free.iter()
                    .position(|&r| phys_free_over(&occ, r, interval.start, interval.end))
            });

        if let Some(free_idx) = free_idx {
            let reg = free.remove(free_idx);
            assignments.insert(interval.vreg, reg);
            active.push((reg, interval.end, interval.vreg));
            active.sort_by_key(|&(_, end, _)| end);
            if is_callee_saved(reg) {
                callee_saved_used.insert(reg);
            }
            continue;
        }

        // No free register works. Evict the active interval that ends
        // furthest among those whose register is fixed-free over our
        // range; take it if it outlives us, otherwise spill ourselves.
        let victim = active
            .iter()
            .enumerate()
            .filter(|(_, &(reg, _, _))| phys_free_over(&occ, reg, interval.start, interval.end))
            .max_by_key(|(_, &(_, end, _))| end)
            .map(|(idx, &entry)| (idx, entry));

        if let Some((vidx, (spill_reg, spill_end, victim_vreg))) = victim {
            if spill_end > interval.end {
                let vclass = vreg_class
                    .get(&victim_vreg)
                    .copied()
                    .unwrap_or(X86RegClass::Gp64);
                let (sz, al) = spill_slot_size(vclass);
                let slot = f.alloc_frame_slot(sz, al);
                spills.insert(victim_vreg, slot);
                assignments.remove(&victim_vreg);
                assignments.insert(interval.vreg, spill_reg);
                active.remove(vidx);
                active.push((spill_reg, interval.end, interval.vreg));
                active.sort_by_key(|&(_, end, _)| end);
                if is_callee_saved(spill_reg) {
                    callee_saved_used.insert(spill_reg);
                }
                continue;
            }
        }

        let (sz, al) = spill_slot_size(interval.class);
        let slot = f.alloc_frame_slot(sz, al);
        spills.insert(interval.vreg, slot);
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
            // At most two spilled operands per class fit the 2 scratch
            // registers. Every current isel shape stays within that
            // (the naive allocator proves it for ALL vregs, and spilled
            // is a subset). Assert loudly rather than silently aliasing
            // a third spill onto scratch[1] — a silent wrong answer is
            // worse than a panic.
            let next_scratch = |class: X86RegClass, gp: &mut usize, fp: &mut usize| -> X86Reg {
                match class {
                    X86RegClass::Xmm | X86RegClass::Xmm128 => {
                        assert!(
                            *fp < FP_SCRATCH.len(),
                            "x86 linear scan: >2 spilled FP operands"
                        );
                        let r = FP_SCRATCH[*fp];
                        *fp += 1;
                        r
                    }
                    _ => {
                        assert!(
                            *gp < GP_SCRATCH.len(),
                            "x86 linear scan: >2 spilled GP operands"
                        );
                        let r = GP_SCRATCH[*gp];
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

    coalesce_self_moves(f);
}

/// Drop register-to-register moves that became `mov P, P` after
/// allocation — the payoff of hinting a vreg toward the register isel
/// moves it to/from. A self-move preserves the value of P for any width
/// (a narrower `movl %eax,%eax` also zero-extends, but a 32-bit vreg's
/// upper bits are never observed without an explicit widen), so the
/// drop is value-preserving. Gated by the full matrix + the class_star
/// valgrind check (the X64-O1-002 zero-extension reproducer).
fn coalesce_self_moves(f: &mut X86Function) {
    for block in &mut f.blocks {
        block.insts.retain(|inst| {
            if !matches!(
                inst.opcode,
                X86Opcode::MovRR | X86Opcode::Movss | X86Opcode::Movsd | X86Opcode::Movaps
            ) {
                return true;
            }
            !matches!(
                (&inst.def, inst.operands.first()),
                (Some(X86Operand::Reg(d)), Some(X86Operand::Reg(s))) if d == s
            )
        });
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
        // The wide pool includes caller-saved registers; only the
        // callee-saved ones that get used must be recorded for save/restore.
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

    #[test]
    fn implicit_effects_div_shift_call() {
        // idiv reads+writes rax and rdx.
        let (u, d) = implicit_phys_effects(X86Opcode::Idiv);
        assert!(u.contains(&X86Reg::Rax) && u.contains(&X86Reg::Rdx));
        assert!(d.contains(&X86Reg::Rax) && d.contains(&X86Reg::Rdx));
        // cltd/cqto: use rax, def rdx.
        let (u, d) = implicit_phys_effects(X86Opcode::Cltd);
        assert_eq!(u, &[X86Reg::Rax]);
        assert_eq!(d, &[X86Reg::Rdx]);
        // A call clobbers every caller-saved register, none callee-saved.
        let (_, d) = implicit_phys_effects(X86Opcode::Call);
        assert!(d.contains(&X86Reg::Rax) && d.contains(&X86Reg::Xmm0));
        assert!(!d.contains(&X86Reg::Rbx) && !d.contains(&X86Reg::R12));
        // A plain ALU op has no implicit physreg effects.
        let (u, d) = implicit_phys_effects(X86Opcode::Add);
        assert!(u.is_empty() && d.is_empty());
    }

    #[test]
    fn phys_free_over_boundaries() {
        let mut occ: HashMap<X86Reg, Vec<u32>> = HashMap::new();
        occ.insert(X86Reg::Rax, vec![4, 10]);
        // [0,2] is before the first occupied position → free.
        assert!(phys_free_over(&occ, X86Reg::Rax, 0, 2));
        // [2,6] straddles position 4 → not free (inclusive).
        assert!(!phys_free_over(&occ, X86Reg::Rax, 2, 6));
        // [5,8] sits between 4 and 10 → free.
        assert!(phys_free_over(&occ, X86Reg::Rax, 5, 8));
        // Exact boundary at an occupied position → not free (conservative).
        assert!(!phys_free_over(&occ, X86Reg::Rax, 4, 4));
        // A register with no occupancy is always free.
        assert!(phys_free_over(&occ, X86Reg::Rbx, 0, 1000));
    }
}
