//! Linear scan register allocator.
//!
//! Replaces the naive spill-everything strategy with proper register assignment.
//! Algorithm: sort live intervals by start, walk in order, assign physical registers.
//! When no register is available, spill the interval ending furthest (or the current
//! one if it ends first). Insert ldr/str for spill code.
//!
//! Callee-saved registers (x19-x28, d8-d15) are tracked — if used, the function
//! prologue/epilogue must save/restore them.

use std::collections::{HashMap, HashSet};
use super::mir::*;
use super::liveness::{LiveInterval, compute_liveness};

/// GP registers available for allocation (excludes x18, x29, x30, x31/sp).
/// Ordered: caller-saved first (prefer these to avoid save/restore overhead),
/// then callee-saved.
// x8 reserved for large-offset addressing in emit.
// x16, x17 excluded (linker scratch, clobbered by dynamic dispatch stubs).
// x9-x11 available — spill code borrows temporarily-free allocated registers,
// falling back to GP_SPILL_SCRATCH only when no allocated register is available.
const GP_ALLOC_ORDER: [u8; 25] = [
    // Caller-saved (temporary, no save needed)
    9, 10, 11, 12, 13, 14, 15,   // x9-x15
    0, 1, 2, 3, 4, 5, 6, 7,      // x0-x7 (args, but available between calls)
    // Callee-saved (must save/restore if used)
    19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
];

// All 32 FP registers available. Spill code borrows temporarily-free registers,
// falling back to FP_SPILL_SCRATCH when no allocated register is available.
const FP_ALLOC_ORDER: [u8; 32] = [
    // Caller-saved
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    0, 1, 2, 3, 4, 5, 6, 7,
    // Callee-saved
    8, 9, 10, 11, 12, 13, 14, 15,
];

/// Callee-saved GP register range.
const GP_CALLEE_SAVED: std::ops::RangeInclusive<u8> = 19..=28;
/// Callee-saved FP register range.
const FP_CALLEE_SAVED: std::ops::RangeInclusive<u8> = 8..=15;

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
    let mut active_gp: Vec<(u8, u32)> = Vec::new(); // (reg_num, interval_end)
    let mut active_fp: Vec<(u8, u32)> = Vec::new();
    let mut free_gp: Vec<u8> = GP_ALLOC_ORDER.to_vec();
    let mut free_fp: Vec<u8> = FP_ALLOC_ORDER.to_vec();
    let mut callee_saved_used: HashSet<PhysReg> = HashSet::new();

    let vreg_classes: HashMap<VRegId, RegClass> = mf.vregs.iter()
        .map(|v| (v.id, v.class))
        .collect();

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
            let callee_range = if is_fp { &FP_CALLEE_SAVED } else { &GP_CALLEE_SAVED };
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
            active.push((reg, interval.end));
            active.sort_by_key(|&(_, end)| end);

            // Track callee-saved usage.
            if !is_fp && GP_CALLEE_SAVED.contains(&reg) {
                callee_saved_used.insert(PhysReg::Gp(reg));
            }
            if is_fp && FP_CALLEE_SAVED.contains(&reg) {
                callee_saved_used.insert(PhysReg::Fp(reg));
            }
        } else {
            // No register available — spill.
            // Find the active interval ending furthest.
            if let Some(last_idx) = active.iter().position(|&(_, end)| end == active.last().map(|a| a.1).unwrap_or(0)) {
                let (spill_reg, spill_end) = active[last_idx];
                if spill_end > interval.end {
                    // Spill the furthest active interval, give its register to us.
                    let victim_vreg = find_vreg_with_reg(&assignments, spill_reg, is_fp);
                    if let Some(victim) = victim_vreg {
                        let offset = mf.alloc_local(8);
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
                        active.push((spill_reg, interval.end));
                        active.sort_by_key(|&(_, end)| end);
                    } else {
                        // Can't find victim — spill current.
                        let offset = mf.alloc_local(8);
                        spills.insert(interval.vreg, offset);
                    }
                } else {
                    // Current interval ends later — spill it.
                    let offset = mf.alloc_local(8);
                    spills.insert(interval.vreg, offset);
                }
            } else {
                // No active intervals — shouldn't happen but spill to be safe.
                let offset = mf.alloc_local(8);
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

    AllocResult { assignments, spills, callee_saved_used: callee_saved }
}

/// Apply allocation result: rewrite VReg operands to PhysReg, insert spill code.
/// For spilled operands, borrows a temporarily-free register from the allocation
/// pool rather than using dedicated scratch registers. This avoids wasting
/// registers in the pool and follows standard linear scan practice.
pub fn apply_allocation(mf: &mut MachineFunction, result: &AllocResult, liveness: &super::liveness::LivenessResult) {
    let vreg_classes: HashMap<VRegId, RegClass> = mf.vregs.iter()
        .map(|v| (v.id, v.class))
        .collect();

    // Build a map from vreg → physical register for assigned vregs.
    // We use this to determine which physical registers are "in use" at each point.
    let assigned_regs: HashSet<u8> = result.assignments.values().filter_map(|p| match p {
        PhysReg::Gp(n) | PhysReg::Gp32(n) => Some(*n),
        _ => None,
    }).collect();

    // Build interval lookup: for each vreg, its live range.
    let intervals: HashMap<VRegId, (u32, u32)> = liveness.intervals.iter()
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
            let mut gp_temps: Vec<u8> = Vec::new();
            let mut fp_temps: Vec<u8> = Vec::new();
            for (&vreg, phys) in &result.assignments {
                if let Some(&(start, end)) = intervals.get(&vreg) {
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
            for &s in &GP_SPILL_SCRATCH { if !gp_temps.contains(&s) { gp_temps.push(s); } }
            for &s in &FP_SPILL_SCRATCH { if !fp_temps.contains(&s) { fp_temps.push(s); } }

            // For spilled vregs used as inputs: borrow a temp register.
            let mut loads = Vec::new();
            let mut gp_temp_idx = 0usize;
            let mut fp_temp_idx = 0usize;
            for (i, op) in inst.operands.iter().enumerate() {
                if let MachineOperand::VReg(vid) = op {
                    if let Some(&offset) = result.spills.get(vid) {
                        let class = vreg_classes.get(vid).copied().unwrap_or(RegClass::Gp64);
                        let (temp_reg, load_op) = if matches!(class, RegClass::Fp32 | RegClass::Fp64) {
                            let r = fp_temps.get(fp_temp_idx).copied().unwrap_or(FP_SPILL_SCRATCH[0]);
                            fp_temp_idx += 1;
                            let phys = if matches!(class, RegClass::Fp32) { PhysReg::Fp32(r) } else { PhysReg::Fp(r) };
                            (phys, ArmOpcode::LdrFpImm)
                        } else {
                            let r = gp_temps.get(gp_temp_idx).copied().unwrap_or(GP_SPILL_SCRATCH[0]);
                            gp_temp_idx += 1;
                            let phys = if matches!(class, RegClass::Gp32) { PhysReg::Gp32(r) } else { PhysReg::Gp(r) };
                            (phys, ArmOpcode::LdrImm)
                        };
                        loads.push((i, temp_reg, load_op, offset));
                    }
                }
            }

            for (op_idx, scratch, load_op, offset) in &loads {
                if *op_idx == 0 && inst.def.as_ref().map(|d| result.spills.contains_key(d)).unwrap_or(false) {
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
                        let r = fp_temps.get(def_temp_idx).copied().unwrap_or(FP_SPILL_SCRATCH[0]);
                        if matches!(class, RegClass::Fp32) { PhysReg::Fp32(r) } else { PhysReg::Fp(r) }
                    } else {
                        let r = gp_temps.get(def_temp_idx).copied().unwrap_or(GP_SPILL_SCRATCH[0]);
                        if matches!(class, RegClass::Gp32) { PhysReg::Gp32(r) } else { PhysReg::Gp(r) }
                    };
                    let scratch = temp_reg;
                    // Replace def operand with scratch.
                    if let Some(MachineOperand::VReg(vid)) = rewritten.operands.first() {
                        if vid == def_vid {
                            rewritten.operands[0] = MachineOperand::PhysReg(scratch);
                        }
                    }
                    Some((scratch, offset, class))
                } else { None }
            } else { None };

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
    let prologue_end = mf.blocks[0].insts.iter().position(|i| {
        // The prologue is StpPre followed by AddImm. Insert after those.
        !matches!(i.opcode, ArmOpcode::StpPre | ArmOpcode::AddImm)
    }).unwrap_or(0);

    let mut saves = Vec::new();
    for &(reg, offset) in &save_slots {
        let (store_op, store_reg) = match reg {
            PhysReg::Fp(_) | PhysReg::Fp32(_) => (ArmOpcode::StrFpImm, reg),
            _ => (ArmOpcode::StrImm, reg),
        };
        saves.push(MachineInst {
            opcode: store_op,
            operands: vec![
                MachineOperand::PhysReg(store_reg),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(offset as i64),
            ],
            def: None,
        });
    }
    // Insert saves at the prologue end.
    for (i, save) in saves.into_iter().enumerate() {
        mf.blocks[0].insts.insert(prologue_end + i, save);
    }

    // Insert restores before every epilogue sequence (LdpPost).
    // The epilogue is LdpPost + ADD sp + RET. Restores must go before LdpPost
    // because LdpPost restores x29 (FP) to the caller's value.
    for block in &mut mf.blocks {
        let mut insertions = Vec::new();
        for (i, inst) in block.insts.iter().enumerate() {
            if inst.opcode == ArmOpcode::LdpPost {
                insertions.push(i);
            }
        }
        for &ldp_idx in insertions.iter().rev() {
            let mut restores = Vec::new();
            for &(reg, offset) in save_slots.iter().rev() {
                let (load_op, load_reg) = match reg {
                    PhysReg::Fp(_) | PhysReg::Fp32(_) => (ArmOpcode::LdrFpImm, reg),
                    _ => (ArmOpcode::LdrImm, reg),
                };
                restores.push(MachineInst {
                    opcode: load_op,
                    operands: vec![
                        MachineOperand::PhysReg(load_reg),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(offset as i64),
                    ],
                    def: None,
                });
            }
            for (j, restore) in restores.into_iter().enumerate() {
                block.insts.insert(ldp_idx, restore);
            }
        }
    }
}

/// Basic move coalescing: eliminate mov instructions where src == dest.
pub fn coalesce_moves(mf: &mut MachineFunction) {
    for block in &mut mf.blocks {
        block.insts.retain(|inst| {
            if matches!(inst.opcode, ArmOpcode::MovReg | ArmOpcode::FmovReg) {
                if inst.operands.len() == 2 && inst.operands[0] == inst.operands[1] {
                    return false; // eliminate self-move
                }
            }
            true
        });
    }
}

// ---- Helpers ----

fn expire_intervals(active: &mut Vec<(u8, u32)>, free: &mut Vec<u8>, pos: u32) {
    let mut i = 0;
    while i < active.len() {
        if active[i].1 < pos {
            let (reg, _) = active.remove(i);
            free.push(reg);
        } else {
            i += 1;
        }
    }
}

fn find_vreg_with_reg(assignments: &HashMap<VRegId, PhysReg>, reg_num: u8, is_fp: bool) -> Option<VRegId> {
    assignments.iter().find_map(|(&vreg, phys)| {
        let num = match phys {
            PhysReg::Gp(n) | PhysReg::Gp32(n) => { if !is_fp { Some(*n) } else { None } }
            PhysReg::Fp(n) | PhysReg::Fp32(n) => { if is_fp { Some(*n) } else { None } }
            _ => None,
        };
        if num == Some(reg_num) { Some(vreg) } else { None }
    })
}

/// Scratch registers for spill code. Multiple scratches needed when an
/// instruction has multiple spilled operands.
const GP_SPILL_SCRATCH: [u8; 3] = [9, 10, 11];
const FP_SPILL_SCRATCH: [u8; 3] = [29, 30, 31];

fn spill_scratch(class: RegClass, idx: usize) -> (PhysReg, ArmOpcode) {
    match class {
        RegClass::Fp64 => (PhysReg::Fp(FP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrFpImm),
        RegClass::Fp32 => (PhysReg::Fp32(FP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrFpImm),
        RegClass::Gp32 => (PhysReg::Gp32(GP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrImm),
        RegClass::Gp64 => (PhysReg::Gp(GP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrImm),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::*;
    use crate::ir::inst::*;
    use crate::ir::builder::FuncBuilder;
    use crate::codegen::isel::select_function;

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
        assert!(!result.assignments.is_empty(), "should have register assignments");
        // No spills needed for 3 vregs.
        assert!(result.spills.is_empty(), "should not spill with only 3 vregs");
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
        assert_eq!(mf.blocks[0].insts.len(), 1, "self-move should be eliminated");
    }
}
