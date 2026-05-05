//! Linear scan register allocator.
//!
//! Replaces the naive spill-everything strategy with proper register assignment.
//! Algorithm: sort live intervals by start, walk in order, assign physical registers.
//! When no register is available, spill the interval ending furthest (or the current
//! one if it ends first). Insert ldr/str for spill code.
//!
//! Callee-saved registers (x19-x28, d8-d15) are tracked — if used, the function
//! prologue/epilogue must save/restore them.

use super::liveness::compute_liveness;
use super::mir::*;
use std::collections::{HashMap, HashSet};

/// GP registers available for allocation (excludes x18, x29, x30, x31/sp).
/// Ordered: caller-saved first (prefer these to avoid save/restore overhead),
/// then callee-saved.
///
/// x8 is reserved for large-offset addressing during emission.
/// x16, x17 are the linker scratch registers (clobbered by dynamic
/// dispatch stubs).
///
/// x9, x10, x11 are reserved **exclusively** for spill reloads
/// (see GP_SPILL_SCRATCH). They used to be in the allocation pool
/// under the theory that spill code could "borrow" them when
/// they're temporarily free, but that assumption is only valid
/// when the reload is inserted at a point where the borrowed
/// register is dead — and the spill code had no reliable way to
/// determine that. The result was a reload clobbering a
/// freshly-computed live value (the slice-print crash at -O1+).
/// Pinning x9-x11 out of the allocation pool costs 3 vregs worth
/// of pressure and guarantees reloads never clash.
const GP_ALLOC_ORDER: [u8; 22] = [
    // Caller-saved (temporary, no save needed)
    12, 13, 14, 15, // x12-x15
    0, 1, 2, 3, 4, 5, 6, 7, // x0-x7 (args, but available between calls)
    // Callee-saved (must save/restore if used)
    19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
];

/// FP registers available for allocation. Same rationale as GP:
/// d29, d30, d31 are reserved exclusively as spill reload scratch.
const FP_ALLOC_ORDER: [u8; 29] = [
    // Caller-saved
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 0, 1, 2, 3, 4, 5, 6, 7,
    // Callee-saved
    8, 9, 10, 11, 12, 13, 14, 15,
];

/// Callee-saved GP register range.
const GP_CALLEE_SAVED: std::ops::RangeInclusive<u8> = 19..=28;
/// Callee-saved FP register range.
const FP_CALLEE_SAVED: std::ops::RangeInclusive<u8> = 8..=15;

/// Bytes a single spill of a vreg of class `class` occupies. NEON
/// vectors need 16; everything else fits in 8. Used to size frame
/// slots so the LdrQ/StrQ pair on a V128 spill operates on a slot
/// that's actually 16 bytes wide and 16-byte aligned.
fn spill_slot_size(class: RegClass) -> u32 {
    match class {
        RegClass::V128 => 16,
        _ => 8,
    }
}

/// Result of register allocation.
pub struct AllocResult {
    /// VRegId → assigned PhysReg (None if spilled).
    pub assignments: HashMap<VRegId, PhysReg>,
    /// Spilled vregs → stack frame offset.
    pub spills: HashMap<VRegId, i32>,
    /// Callee-saved registers that were used and need save/restore.
    pub callee_saved_used: Vec<PhysReg>,
}

/// Run linear scan register allocation on a machine function.
pub fn linear_scan(mf: &mut MachineFunction) -> AllocResult {
    let liveness = compute_liveness(mf);

    let mut assignments: HashMap<VRegId, PhysReg> = HashMap::new();
    let mut spills: HashMap<VRegId, i32> = HashMap::new();
    // Active intervals: (reg_num, interval_end, current_vreg). The
    // vreg is tracked here so spill victim selection can identify
    // the *current* holder of a physical register without iterating
    // the `assignments` HashMap (which accumulates stale historical
    // entries when registers are reused after expiry, and whose
    // iteration order is non-deterministic). Audit fix surfaced by
    // mem2reg work — produced flaky `select_type.f90` failures and
    // non-reproducible builds across compiles.
    let mut active_gp: Vec<(u8, u32, VRegId)> = Vec::new();
    let mut active_fp: Vec<(u8, u32, VRegId)> = Vec::new();
    let mut free_gp: Vec<u8> = GP_ALLOC_ORDER.to_vec();
    let mut free_fp: Vec<u8> = FP_ALLOC_ORDER.to_vec();
    let mut callee_saved_used: HashSet<PhysReg> = HashSet::new();

    for interval in &liveness.intervals {
        let is_fp = matches!(interval.class, RegClass::Fp32 | RegClass::Fp64);

        // Expire old intervals whose end < current start.
        if is_fp {
            expire_intervals(&mut active_fp, &mut free_fp, interval.start);
        } else {
            expire_intervals(&mut active_gp, &mut free_gp, interval.start);
        }

        // Try to assign a register.
        // If the interval crosses a call, ONLY use callee-saved registers.
        // Caller-saved registers would be clobbered by the call.
        let (active, free) = if is_fp {
            (&mut active_fp, &mut free_fp)
        } else {
            (&mut active_gp, &mut free_gp)
        };

        let reg_opt = if interval.crosses_call {
            // Must use callee-saved. Find one in the free list.
            let callee_range = if is_fp {
                &FP_CALLEE_SAVED
            } else {
                &GP_CALLEE_SAVED
            };
            let idx = free.iter().position(|r| callee_range.contains(r));
            idx.map(|i| free.remove(i))
        } else if let Some(hint) = interval.hint {
            // Try the hinted register first (reduces unnecessary moves).
            let idx = free.iter().position(|&r| r == hint);
            if let Some(i) = idx {
                Some(free.remove(i))
            } else {
                free.pop() // hint unavailable, use any free
            }
        } else {
            free.pop()
        };

        if let Some(reg) = reg_opt {
            // Register available.
            let phys = if is_fp {
                match interval.class {
                    RegClass::Fp32 => PhysReg::Fp32(reg),
                    _ => PhysReg::Fp(reg),
                }
            } else {
                match interval.class {
                    RegClass::Gp32 => PhysReg::Gp32(reg),
                    _ => PhysReg::Gp(reg),
                }
            };
            assignments.insert(interval.vreg, phys);
            active.push((reg, interval.end, interval.vreg));
            active.sort_by_key(|&(_, end, _)| end);

            // Track callee-saved usage.
            if !is_fp && GP_CALLEE_SAVED.contains(&reg) {
                callee_saved_used.insert(PhysReg::Gp(reg));
            }
            if is_fp && FP_CALLEE_SAVED.contains(&reg) {
                callee_saved_used.insert(PhysReg::Fp(reg));
            }
        } else {
            // No register available — spill. Find the active
            // interval ending furthest. `active` is kept sorted by
            // end (see the `active.sort_by_key(...)` calls above
            // and below), so the last entry is always the furthest.
            // The active list carries the current vreg directly so
            // we don't have to scan the (non-deterministic,
            // stale-entry-laden) assignments map for the victim.
            if let Some(&(spill_reg, spill_end, victim)) = active.last() {
                let last_idx = active.len() - 1;
                if spill_end > interval.end {
                    // Spill the furthest active interval, give its register to us.
                    // The victim's class is what determines its slot size — not
                    // ours, since we're taking the victim's physreg.
                    let victim_class = mf
                        .vregs
                        .iter()
                        .find(|v| v.id == victim)
                        .map(|v| v.class)
                        .unwrap_or(RegClass::Gp64);
                    let offset = mf.alloc_local(spill_slot_size(victim_class));
                    spills.insert(victim, offset);
                    assignments.remove(&victim);

                    let phys = if is_fp {
                        match interval.class {
                            RegClass::Fp32 => PhysReg::Fp32(spill_reg),
                            _ => PhysReg::Fp(spill_reg),
                        }
                    } else {
                        match interval.class {
                            RegClass::Gp32 => PhysReg::Gp32(spill_reg),
                            _ => PhysReg::Gp(spill_reg),
                        }
                    };
                    assignments.insert(interval.vreg, phys);
                    active.remove(last_idx);
                    active.push((spill_reg, interval.end, interval.vreg));
                    active.sort_by_key(|&(_, end, _)| end);
                } else {
                    // Current interval ends later — spill it. Use the
                    // current interval's class for slot sizing.
                    let offset = mf.alloc_local(spill_slot_size(interval.class));
                    spills.insert(interval.vreg, offset);
                }
            } else {
                // No active intervals — shouldn't happen but spill to be safe.
                let offset = mf.alloc_local(spill_slot_size(interval.class));
                spills.insert(interval.vreg, offset);
            }
        }
    }

    // Sort callee-saved for consistent prologue/epilogue ordering.
    let mut callee_saved: Vec<PhysReg> = callee_saved_used.into_iter().collect();
    callee_saved.sort_by_key(|r| match r {
        PhysReg::Gp(n) | PhysReg::Gp32(n) => *n as u16,
        PhysReg::Fp(n) | PhysReg::Fp32(n) => 100 + *n as u16,
        _ => 200,
    });

    AllocResult {
        assignments,
        spills,
        callee_saved_used: callee_saved,
    }
}

/// Apply allocation result: rewrite VReg operands to PhysReg, insert spill code.
/// For spilled operands, borrows a temporarily-free register from the allocation
/// pool rather than using dedicated scratch registers. This avoids wasting
/// registers in the pool and follows standard linear scan practice.
pub fn apply_allocation(
    mf: &mut MachineFunction,
    result: &AllocResult,
    liveness: &super::liveness::LivenessResult,
) {
    let vreg_classes: HashMap<VRegId, RegClass> =
        mf.vregs.iter().map(|v| (v.id, v.class)).collect();

    // Build interval lookup: for each vreg, its live range.
    let intervals: HashMap<VRegId, (u32, u32)> = liveness
        .intervals
        .iter()
        .map(|i| (i.vreg, (i.start, i.end)))
        .collect();

    // Compute instruction positions for each block/instruction.
    let mut inst_pos: HashMap<(usize, usize), u32> = HashMap::new();
    let mut pos: u32 = 0;
    for (bi, block) in mf.blocks.iter().enumerate() {
        for (ii, _) in block.insts.iter().enumerate() {
            inst_pos.insert((bi, ii), pos);
            pos += 2;
        }
    }

    for block_idx in 0..mf.blocks.len() {
        let mut new_insts = Vec::new();
        let insts = std::mem::take(&mut mf.blocks[block_idx].insts);

        for (inst_idx, inst) in insts.iter().enumerate() {
            let mut rewritten = inst.clone();
            let cur_pos = inst_pos.get(&(block_idx, inst_idx)).copied().unwrap_or(0);

            // Find GP registers NOT occupied by any live interval at this position.
            // These are safe to use as temporary spill registers.
            //
            // Iterate `result.assignments` in **deterministic vreg
            // order**, not raw HashMap iteration order — the latter
            // varies between runs and produces different temp-reg
            // assignments and therefore non-reproducible builds. The
            // resulting `gp_temps`/`fp_temps` lists are consumed by
            // index, so their order is load-bearing.
            let mut gp_temps: Vec<u8> = Vec::new();
            let mut fp_temps: Vec<u8> = Vec::new();
            let mut sorted_assignments: Vec<(VRegId, PhysReg)> =
                result.assignments.iter().map(|(&v, &p)| (v, p)).collect();
            sorted_assignments.sort_by_key(|(v, _)| v.0);
            for (vreg, phys) in &sorted_assignments {
                if let Some(&(start, end)) = intervals.get(vreg) {
                    if cur_pos < start || cur_pos > end {
                        // This vreg's register is free at this point.
                        match phys {
                            PhysReg::Gp(n) | PhysReg::Gp32(n) => gp_temps.push(*n),
                            PhysReg::Fp(n) | PhysReg::Fp32(n) => fp_temps.push(*n),
                            _ => {}
                        }
                    }
                }
            }
            // Also include the dedicated fallback scratches in case no free regs found.
            for &s in &GP_SPILL_SCRATCH {
                if !gp_temps.contains(&s) {
                    gp_temps.push(s);
                }
            }
            for &s in &FP_SPILL_SCRATCH {
                if !fp_temps.contains(&s) {
                    fp_temps.push(s);
                }
            }

            // For spilled vregs used as inputs: borrow a temp register.
            let mut loads = Vec::new();
            let mut gp_temp_idx = 0usize;
            let mut fp_temp_idx = 0usize;
            for (i, op) in inst.operands.iter().enumerate() {
                if let MachineOperand::VReg(vid) = op {
                    if let Some(&offset) = result.spills.get(vid) {
                        let class = vreg_classes.get(vid).copied().unwrap_or(RegClass::Gp64);
                        let (temp_reg, load_op) =
                            if matches!(class, RegClass::Fp32 | RegClass::Fp64) {
                                let r = fp_temps
                                    .get(fp_temp_idx)
                                    .copied()
                                    .unwrap_or(FP_SPILL_SCRATCH[0]);
                                fp_temp_idx += 1;
                                let phys = if matches!(class, RegClass::Fp32) {
                                    PhysReg::Fp32(r)
                                } else {
                                    PhysReg::Fp(r)
                                };
                                (phys, ArmOpcode::LdrFpImm)
                            } else {
                                let r = gp_temps
                                    .get(gp_temp_idx)
                                    .copied()
                                    .unwrap_or(GP_SPILL_SCRATCH[0]);
                                gp_temp_idx += 1;
                                let phys = if matches!(class, RegClass::Gp32) {
                                    PhysReg::Gp32(r)
                                } else {
                                    PhysReg::Gp(r)
                                };
                                (phys, ArmOpcode::LdrImm)
                            };
                        loads.push((i, temp_reg, load_op, offset));
                    }
                }
            }

            for (op_idx, scratch, load_op, offset) in &loads {
                if *op_idx == 0
                    && inst
                        .def
                        .as_ref()
                        .map(|d| result.spills.contains_key(d))
                        .unwrap_or(false)
                {
                    continue;
                }
                new_insts.push(MachineInst {
                    opcode: *load_op,
                    operands: vec![
                        MachineOperand::PhysReg(*scratch),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(*offset as i64),
                    ],
                    def: None,
                });
                rewritten.operands[*op_idx] = MachineOperand::PhysReg(*scratch);
            }

            // Rewrite assigned vregs to physical registers.
            for op in &mut rewritten.operands {
                if let MachineOperand::VReg(vid) = op {
                    if let Some(phys) = result.assignments.get(vid) {
                        *op = MachineOperand::PhysReg(*phys);
                    }
                }
            }

            // Handle def — use a temp that doesn't alias any input temp.
            let def_temp_idx = gp_temp_idx.max(fp_temp_idx);
            let def_spill = if let Some(def_vid) = &inst.def {
                if let Some(&offset) = result.spills.get(def_vid) {
                    let class = vreg_classes.get(def_vid).copied().unwrap_or(RegClass::Gp64);
                    let temp_reg = if matches!(class, RegClass::Fp32 | RegClass::Fp64) {
                        let r = fp_temps
                            .get(def_temp_idx)
                            .copied()
                            .unwrap_or(FP_SPILL_SCRATCH[0]);
                        if matches!(class, RegClass::Fp32) {
                            PhysReg::Fp32(r)
                        } else {
                            PhysReg::Fp(r)
                        }
                    } else {
                        let r = gp_temps
                            .get(def_temp_idx)
                            .copied()
                            .unwrap_or(GP_SPILL_SCRATCH[0]);
                        if matches!(class, RegClass::Gp32) {
                            PhysReg::Gp32(r)
                        } else {
                            PhysReg::Gp(r)
                        }
                    };
                    let scratch = temp_reg;
                    // Replace def operand with scratch.
                    if let Some(MachineOperand::VReg(vid)) = rewritten.operands.first() {
                        if vid == def_vid {
                            rewritten.operands[0] = MachineOperand::PhysReg(scratch);
                        }
                    }
                    Some((scratch, offset, class))
                } else {
                    None
                }
            } else {
                None
            };

            rewritten.def = None;
            new_insts.push(rewritten);

            // Store after def for spilled vregs.
            if let Some((scratch, offset, class)) = def_spill {
                let store_op = match class {
                    RegClass::Fp32 | RegClass::Fp64 => ArmOpcode::StrFpImm,
                    _ => ArmOpcode::StrImm,
                };
                new_insts.push(MachineInst {
                    opcode: store_op,
                    operands: vec![
                        MachineOperand::PhysReg(scratch),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(offset as i64),
                    ],
                    def: None,
                });
            }
        }

        mf.blocks[block_idx].insts = new_insts;
    }
}

/// Insert callee-saved register saves in prologue and restores in epilogue.
/// Must be called after apply_allocation so we know which callee-saved regs were used.
pub fn insert_callee_saves(mf: &mut MachineFunction, callee_saved: &[PhysReg]) {
    if callee_saved.is_empty() {
        return;
    }

    // Allocate stack slots for callee-saved registers.
    let mut save_slots: Vec<(PhysReg, i32)> = Vec::new();
    for &reg in callee_saved {
        let offset = mf.frame.alloc_local(8);
        save_slots.push((reg, offset));
    }

    // Insert saves at the start of the entry block (after prologue setup).
    // Find the insertion point: after the STP + ADD (prologue) instructions.
    let prologue_end = mf.blocks[0]
        .insts
        .iter()
        .position(|i| {
            // The prologue is StpPre followed by AddImm. Insert after those.
            !matches!(i.opcode, ArmOpcode::StpPre | ArmOpcode::AddImm)
        })
        .unwrap_or(0);

    // Emit saves, pairing consecutive same-class registers into STP.
    // save_slots are ordered by increasing register number; slots are
    // allocated at decreasing offsets (-8, -16, -24, ...). Adjacent
    // slots differ by 8 bytes — exactly the STP stride for GP64/FP64.
    //
    // STP Xt1, Xt2, [Xn, #off]: stores Xt1 at Xn+off, Xt2 at Xn+off+8.
    // So to store reg_low (at lower_offset) and reg_high (at lower_offset+8):
    //   STP reg_low, reg_high, [FP, #lower_offset]
    let saves = emit_callee_store_pairs(&save_slots, false);
    for (i, save) in saves.into_iter().enumerate() {
        mf.blocks[0].insts.insert(prologue_end + i, save);
    }

    // Insert restores before every epilogue sequence (LdpPost).
    for block in &mut mf.blocks {
        let mut insertions = Vec::new();
        for (i, inst) in block.insts.iter().enumerate() {
            if inst.opcode == ArmOpcode::LdpPost {
                insertions.push(i);
            }
        }
        for &ldp_idx in insertions.iter().rev() {
            // Restores in reverse order (mirror of saves).
            let restores = emit_callee_store_pairs(&save_slots, true);
            for restore in restores.into_iter().rev() {
                block.insts.insert(ldp_idx, restore);
            }
        }
    }
}

/// Build the save (or restore) instruction list for a set of callee-saved
/// slots, pairing consecutive adjacent GP/FP slots into STP/LDP.
///
/// `restore` = true → emit LDP/LDR (load); false → STP/STR (store).
///
/// Pairing rule: slots[i] and slots[i+1] are paired when:
///   * Both are GP or both are FP (same register class)
///   * slots[i+1].offset == slots[i].offset - 8 (adjacent 8-byte slots)
///
/// STP Xt1, Xt2, [Xn, #off] → Xt1 at off, Xt2 at off+8.
/// We pair as: STP slots[i+1].reg, slots[i].reg, [FP, #slots[i+1].offset]
/// because slots[i+1] has the lower (more negative) offset.
fn emit_callee_store_pairs(save_slots: &[(PhysReg, i32)], restore: bool) -> Vec<MachineInst> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < save_slots.len() {
        let (reg1, off1) = save_slots[i];
        // Try to pair with next slot.
        if i + 1 < save_slots.len() {
            let (reg2, off2) = save_slots[i + 1];
            let is_gp1 = !matches!(reg1, PhysReg::Fp(_) | PhysReg::Fp32(_));
            let is_gp2 = !matches!(reg2, PhysReg::Fp(_) | PhysReg::Fp32(_));
            let same_class = is_gp1 == is_gp2;
            // off1 is the higher slot (less negative); off2 = off1 - 8.
            let adjacent = off2 == off1 - 8;
            // STP offset must fit in 7-bit signed × 8: range -512..504.
            let in_range = (-512..=504).contains(&off2);
            if same_class && adjacent && in_range {
                let opcode = if restore {
                    ArmOpcode::LdpOffset
                } else {
                    ArmOpcode::StpOffset
                };
                let (low_reg, high_reg) = (reg2, reg1);
                result.push(MachineInst {
                    opcode,
                    operands: vec![
                        MachineOperand::PhysReg(low_reg),
                        MachineOperand::PhysReg(high_reg),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(off2 as i64),
                    ],
                    def: None,
                });
                i += 2;
                continue;
            }
        }
        // Emit individual STR/LDR.
        let (op, is_fp) = match reg1 {
            PhysReg::Fp(_) | PhysReg::Fp32(_) => {
                if restore {
                    (ArmOpcode::LdrFpImm, true)
                } else {
                    (ArmOpcode::StrFpImm, true)
                }
            }
            _ => {
                if restore {
                    (ArmOpcode::LdrImm, false)
                } else {
                    (ArmOpcode::StrImm, false)
                }
            }
        };
        let _ = is_fp;
        result.push(MachineInst {
            opcode: op,
            operands: vec![
                MachineOperand::PhysReg(reg1),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(off1 as i64),
            ],
            def: if restore {
                Some(match reg1 {
                    PhysReg::Gp(n) | PhysReg::Gp32(n) => crate::codegen::mir::VRegId(n.into()),
                    PhysReg::Fp(n) | PhysReg::Fp32(n) => crate::codegen::mir::VRegId(n.into()),
                    _ => crate::codegen::mir::VRegId(0),
                })
            } else {
                None
            },
        });
        i += 1;
    }
    result
}

/// Basic move coalescing: eliminate mov instructions where src == dest.
pub fn coalesce_moves(mf: &mut MachineFunction) {
    for block in &mut mf.blocks {
        block.insts.retain(|inst| {
            if matches!(inst.opcode, ArmOpcode::MovReg | ArmOpcode::FmovReg)
                && inst.operands.len() == 2
                && inst.operands[0] == inst.operands[1]
            {
                return false; // eliminate self-move
            }
            true
        });
    }
}

/// Reorder physical argument-register copies immediately before calls so later
/// sources are not clobbered by earlier destination writes.
pub fn parallelize_call_arg_moves(mf: &mut MachineFunction) {
    for block in &mut mf.blocks {
        let mut rebuilt: Vec<MachineInst> = Vec::with_capacity(block.insts.len());
        for inst in std::mem::take(&mut block.insts) {
            if matches!(inst.opcode, ArmOpcode::Bl | ArmOpcode::Blr) {
                let mut start = rebuilt.len();
                while start > 0 && is_call_arg_copy(&rebuilt[start - 1]) {
                    start -= 1;
                }
                if start < rebuilt.len() {
                    let pending = rebuilt.split_off(start);
                    rebuilt.extend(rewrite_call_arg_copies(pending));
                }
                rebuilt.push(inst);
            } else {
                rebuilt.push(inst);
            }
        }
        block.insts = rebuilt;
    }
}

// ---- Helpers ----

fn is_call_arg_copy(inst: &MachineInst) -> bool {
    matches!(inst.opcode, ArmOpcode::MovReg | ArmOpcode::FmovReg)
        && matches!(
            inst.operands.as_slice(),
            [MachineOperand::PhysReg(dst), MachineOperand::PhysReg(src)]
                if is_call_arg_reg(*dst) && !matches!(src, PhysReg::Xzr | PhysReg::Wzr)
        )
}

fn is_call_arg_reg(reg: PhysReg) -> bool {
    match reg {
        PhysReg::Gp(n) | PhysReg::Gp32(n) => n < 8,
        PhysReg::Fp(n) | PhysReg::Fp32(n) => n < 8,
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PhysRegAlias {
    Gp(u8),
    Fp(u8),
}

fn phys_reg_alias(reg: PhysReg) -> Option<PhysRegAlias> {
    match reg {
        PhysReg::Gp(n) | PhysReg::Gp32(n) => Some(PhysRegAlias::Gp(n)),
        PhysReg::Fp(n) | PhysReg::Fp32(n) => Some(PhysRegAlias::Fp(n)),
        _ => None,
    }
}

fn scratch_phys_for(reg: PhysReg) -> PhysReg {
    match reg {
        PhysReg::Gp(_) => PhysReg::Gp(9),
        PhysReg::Gp32(_) => PhysReg::Gp32(9),
        PhysReg::Fp(_) => PhysReg::Fp(29),
        PhysReg::Fp32(_) => PhysReg::Fp32(29),
        _ => panic!("call-arg scratch requested for non-register operand"),
    }
}

fn move_opcode_for_phys(reg: PhysReg) -> ArmOpcode {
    match reg {
        PhysReg::Fp(_) | PhysReg::Fp32(_) => ArmOpcode::FmovReg,
        PhysReg::Gp(_) | PhysReg::Gp32(_) => ArmOpcode::MovReg,
        _ => panic!("move opcode requested for non-register operand"),
    }
}

fn rewrite_call_arg_copies(pending_moves: Vec<MachineInst>) -> Vec<MachineInst> {
    let mut pending: Vec<(ArmOpcode, PhysReg, PhysReg)> = pending_moves
        .into_iter()
        .map(|inst| match inst.operands.as_slice() {
            [MachineOperand::PhysReg(dst), MachineOperand::PhysReg(src)] => {
                (inst.opcode, *dst, *src)
            }
            _ => panic!("call-arg copy rewrite saw unexpected operand shape"),
        })
        .collect();
    let mut rewritten = Vec::with_capacity(pending.len() + 1);

    while !pending.is_empty() {
        let safe_idx = (0..pending.len()).find(|&i| {
            let (_, dst, _) = pending[i];
            let dst_alias = phys_reg_alias(dst).expect("call-arg copy dst should alias");
            !pending
                .iter()
                .enumerate()
                .any(|(j, &(_, _, src))| j != i && phys_reg_alias(src) == Some(dst_alias))
        });

        if let Some(idx) = safe_idx {
            let (opcode, dst, src) = pending.remove(idx);
            rewritten.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::PhysReg(dst), MachineOperand::PhysReg(src)],
                def: None,
            });
            continue;
        }

        let (_, _, src) = pending[0];
        let scratch = scratch_phys_for(src);
        rewritten.push(MachineInst {
            opcode: move_opcode_for_phys(src),
            operands: vec![MachineOperand::PhysReg(scratch), MachineOperand::PhysReg(src)],
            def: None,
        });
        pending[0].2 = scratch;
    }

    rewritten
}

fn expire_intervals(active: &mut Vec<(u8, u32, VRegId)>, free: &mut Vec<u8>, pos: u32) {
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

/// Scratch registers for spill code. Multiple scratches needed when an
/// instruction has multiple spilled operands.
const GP_SPILL_SCRATCH: [u8; 3] = [9, 10, 11];
const FP_SPILL_SCRATCH: [u8; 3] = [29, 30, 31];

fn spill_scratch(class: RegClass, idx: usize) -> (PhysReg, ArmOpcode) {
    match class {
        RegClass::Fp64 => (PhysReg::Fp(FP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrFpImm),
        RegClass::Fp32 => (
            PhysReg::Fp32(FP_SPILL_SCRATCH[idx % 3]),
            ArmOpcode::LdrFpImm,
        ),
        // 128-bit vector spills/fills via LdrQ — same FP scratch
        // bank (the V registers ARE the 128-bit form of the same
        // physical D/S registers).
        RegClass::V128 => (PhysReg::Fp(FP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrQ),
        RegClass::Gp32 => (PhysReg::Gp32(GP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrImm),
        RegClass::Gp64 => (PhysReg::Gp(GP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrImm),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::isel::select_function;
    use crate::ir::builder::FuncBuilder;
    use crate::ir::inst::*;
    use crate::ir::types::*;

    #[test]
    fn linear_scan_assigns_registers() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mut mf = select_function(&func);
        let result = linear_scan(&mut mf);
        // Should have assignments for the vregs.
        assert!(
            !result.assignments.is_empty(),
            "should have register assignments"
        );
        // No spills needed for 3 vregs.
        assert!(
            result.spills.is_empty(),
            "should not spill with only 3 vregs"
        );
    }

    #[test]
    fn linear_scan_no_x18() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            // Create many vregs to exercise the allocator.
            let mut vals = Vec::new();
            for i in 0..20 {
                vals.push(b.const_i32(i));
            }
            // Use them all.
            let mut sum = vals[0];
            for &v in &vals[1..] {
                sum = b.iadd(sum, v);
            }
            b.ret_void();
        }
        let mut mf = select_function(&func);
        let result = linear_scan(&mut mf);
        // x18 must never be assigned.
        for phys in result.assignments.values() {
            match phys {
                PhysReg::Gp(18) | PhysReg::Gp32(18) => {
                    panic!("x18 assigned — platform reserved!");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn coalesce_eliminates_self_moves() {
        let mut mf = MachineFunction::new("test".into());
        let reg = PhysReg::Gp(9);
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![MachineOperand::PhysReg(reg), MachineOperand::PhysReg(reg)],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Ret,
            operands: vec![],
            def: None,
        });
        assert_eq!(mf.blocks[0].insts.len(), 2);
        coalesce_moves(&mut mf);
        assert_eq!(
            mf.blocks[0].insts.len(),
            1,
            "self-move should be eliminated"
        );
    }

    #[test]
    fn parallelize_call_arg_moves_preserves_later_source_registers() {
        let mut mf = MachineFunction::new("test".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(28)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::PhysReg(PhysReg::Gp(12))],
            def: None,
        });

        parallelize_call_arg_moves(&mut mf);

        assert_eq!(mf.blocks[0].insts.len(), 3);
        assert_eq!(
            mf.blocks[0].insts[0].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[1].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(28)),
            ]
        );
    }

    #[test]
    fn parallelize_call_arg_moves_breaks_cycles_with_scratch() {
        let mut mf = MachineFunction::new("test".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(1)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![MachineOperand::Extern("_callee".into())],
            def: None,
        });

        parallelize_call_arg_moves(&mut mf);

        assert_eq!(mf.blocks[0].insts.len(), 4);
        assert_eq!(
            mf.blocks[0].insts[0].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(9)),
                MachineOperand::PhysReg(PhysReg::Gp(1)),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[1].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[2].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(9)),
            ]
        );
    }

    #[test]
    fn parallelize_call_arg_moves_ignores_zero_materialization_into_arg_regs() {
        let mut mf = MachineFunction::new("test".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(4)),
                MachineOperand::PhysReg(PhysReg::Xzr),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(4)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::PhysReg(PhysReg::Gp(19))],
            def: None,
        });

        parallelize_call_arg_moves(&mut mf);

        assert_eq!(mf.blocks[0].insts.len(), 3);
        assert_eq!(
            mf.blocks[0].insts[0].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(4)),
                MachineOperand::PhysReg(PhysReg::Xzr),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[1].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(4)),
            ]
        );
    }
}
