//! Naive register allocation — spill everything.
//!
//! Every virtual register gets a stack slot. Before each use, load from
//! the slot into a scratch register. After each def, store from the
//! scratch register to the slot. This produces correct but slow code.
//!
//! Scratch registers used:
//! - x9, x10, x11 for integer operands (caller-saved temporaries)
//! - d16, d17, d18 for float operands (caller-saved temporaries)
//!
//! This will be replaced by linear scan allocation in Sprint 21.

use super::mir::*;
use std::collections::HashMap;

/// Integer scratch registers (caller-saved, safe to clobber).
const GP_SCRATCH: [u8; 3] = [9, 10, 11];
/// Float scratch registers (caller-saved, safe to clobber).
const FP_SCRATCH: [u8; 3] = [16, 17, 18];

#[derive(Clone, Copy)]
struct SpillSlot {
    offset: i32,
    size: u32,
}

fn spill_size(class: RegClass) -> u32 {
    match class {
        RegClass::Gp64 | RegClass::Fp64 => 8,
        RegClass::Gp32 | RegClass::Fp32 => 4,
        RegClass::V128 => 16,
    }
}

fn spill_align(size: u32) -> u32 {
    if size >= 16 {
        16
    } else if size >= 8 {
        8
    } else {
        4
    }
}

/// Assign memory to the spill-everything allocator without making every
/// short-lived vreg permanently enlarge the frame.  The liveness intervals
/// are conservative across the CFG, so sharing slots whose intervals are
/// disjoint cannot make values on different paths alias while live.
fn allocate_spill_slots(mf: &mut MachineFunction) -> HashMap<VRegId, i32> {
    let intervals = super::liveness::compute_spill_liveness(mf).intervals;
    let mut slots = HashMap::with_capacity(intervals.len());
    let mut active: Vec<(u32, SpillSlot)> = Vec::new();
    let mut free: Vec<SpillSlot> = Vec::new();

    for interval in intervals {
        // An interval ending at this instruction still overlaps a value
        // defined by the same instruction: the old operands are read before
        // the result is stored.  Only strictly earlier intervals may expire.
        let mut index = 0;
        while index < active.len() {
            if active[index].0 < interval.start {
                let (_, slot) = active.swap_remove(index);
                free.push(slot);
            } else {
                index += 1;
            }
        }

        let size = spill_size(interval.class);
        let align = spill_align(size);
        let reusable = free
            .iter()
            .enumerate()
            .filter(|(_, slot)| {
                slot.size >= size && slot.offset.unsigned_abs().is_multiple_of(align)
            })
            .min_by_key(|(_, slot)| (slot.size, slot.offset.unsigned_abs()))
            .map(|(index, _)| index);
        let slot = if let Some(index) = reusable {
            free.swap_remove(index)
        } else {
            SpillSlot {
                offset: mf.alloc_local(size),
                size,
            }
        };

        slots.insert(interval.vreg, slot.offset);
        active.push((interval.end, slot));
    }

    slots
}

/// Allocate registers for a machine function using spill-everything strategy.
/// Modifies the function in place: replaces VReg operands with PhysReg,
/// inserts loads/stores around each instruction.
pub fn regalloc_naive(mf: &mut MachineFunction) {
    // Phase 1: spill every vreg, but reuse storage after its conservative
    // live interval ends.  This keeps -O0 simple without making large
    // recursive procedures consume tens of kilobytes per call.
    let vreg_slots = allocate_spill_slots(mf);

    // Build class map for quick lookup.
    let vreg_classes: HashMap<VRegId, RegClass> =
        mf.vregs.iter().map(|v| (v.id, v.class)).collect();

    // Phase 2: rewrite each block's instructions.
    for block_idx in 0..mf.blocks.len() {
        let mut new_insts = Vec::new();

        let insts = std::mem::take(&mut mf.blocks[block_idx].insts);
        for inst in insts {
            let mut rewritten = inst.clone();

            // Collect which vreg operands need loading (inputs) and which
            // need storing (output/def).
            let mut loads: Vec<(usize, VRegId)> = Vec::new(); // (operand_idx, vreg)
            let def_vreg = inst.def;

            // Identify vreg operands that are inputs (not the def).
            for (i, op) in inst.operands.iter().enumerate() {
                if let MachineOperand::VReg(vid) = op {
                    // Ordinary operand-zero definitions are write-only. A
                    // destructive definition reads its old destination value
                    // before overwriting it, so its spill slot must be loaded.
                    if Some(*vid) == def_vreg && i == 0 && !inst.opcode.reads_def_operand() {
                        continue;
                    }
                    loads.push((i, *vid));
                }
            }

            // Emit loads for input operands.
            let mut gp_scratch_idx = 0;
            let mut fp_scratch_idx = 0;

            for (op_idx, vid) in &loads {
                if let Some(&offset) = vreg_slots.get(vid) {
                    let class = vreg_classes.get(vid).copied().unwrap_or(RegClass::Gp64);
                    let (scratch, load_op) = match class {
                        RegClass::Fp64 => {
                            let s = FP_SCRATCH[fp_scratch_idx % FP_SCRATCH.len()];
                            fp_scratch_idx += 1;
                            (PhysReg::Fp(s), ArmOpcode::LdrFpImm)
                        }
                        RegClass::Fp32 => {
                            let s = FP_SCRATCH[fp_scratch_idx % FP_SCRATCH.len()];
                            fp_scratch_idx += 1;
                            (PhysReg::Fp32(s), ArmOpcode::LdrFpImm)
                        }
                        RegClass::V128 => {
                            // 128-bit vector spill/fill uses LdrQ / StrQ
                            // off the FP-scratch pool — same physical
                            // register bank as Fp{32,64}, just at 128b
                            // width.
                            let s = FP_SCRATCH[fp_scratch_idx % FP_SCRATCH.len()];
                            fp_scratch_idx += 1;
                            (PhysReg::Fp(s), ArmOpcode::LdrQ)
                        }
                        RegClass::Gp32 => {
                            let s = GP_SCRATCH[gp_scratch_idx % GP_SCRATCH.len()];
                            gp_scratch_idx += 1;
                            (PhysReg::Gp32(s), ArmOpcode::LdrImm)
                        }
                        RegClass::Gp64 => {
                            let s = GP_SCRATCH[gp_scratch_idx % GP_SCRATCH.len()];
                            gp_scratch_idx += 1;
                            (PhysReg::Gp(s), ArmOpcode::LdrImm)
                        }
                    };

                    // LDR scratch, [FP, #offset]
                    new_insts.push(MachineInst {
                        opcode: load_op,
                        operands: vec![
                            MachineOperand::PhysReg(scratch),
                            MachineOperand::PhysReg(PhysReg::FP),
                            MachineOperand::Imm(offset as i64),
                        ],
                        def: None,
                    });

                    // Replace the vreg operand with the scratch register.
                    rewritten.operands[*op_idx] = MachineOperand::PhysReg(scratch);
                }
            }

            // Replace the def operand (first operand if it matches def).
            let def_scratch = if let Some(def_vid) = def_vreg {
                if let Some(&offset) = vreg_slots.get(&def_vid) {
                    let class = vreg_classes
                        .get(&def_vid)
                        .copied()
                        .unwrap_or(RegClass::Gp64);
                    let scratch = match class {
                        RegClass::Fp64 => PhysReg::Fp(FP_SCRATCH[0]),
                        RegClass::Fp32 => PhysReg::Fp32(FP_SCRATCH[0]),
                        RegClass::V128 => PhysReg::Fp(FP_SCRATCH[0]),
                        RegClass::Gp32 => PhysReg::Gp32(GP_SCRATCH[0]),
                        RegClass::Gp64 => PhysReg::Gp(GP_SCRATCH[0]),
                    };

                    // Replace def operand.
                    if let Some(MachineOperand::VReg(vid)) = rewritten.operands.first() {
                        if *vid == def_vid {
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

            // Emit the rewritten instruction.
            rewritten.def = None; // physical regs don't track defs
            new_insts.push(rewritten);

            // Emit store for the def.
            if let Some((scratch, offset, class)) = def_scratch {
                let store_op = match class {
                    RegClass::Fp32 | RegClass::Fp64 => ArmOpcode::StrFpImm,
                    RegClass::V128 => ArmOpcode::StrQ,
                    RegClass::Gp32 | RegClass::Gp64 => ArmOpcode::StrImm,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::isel::select_function;
    use crate::ir::builder::FuncBuilder;
    use crate::ir::inst::*;
    use crate::ir::types::*;

    #[test]
    fn regalloc_replaces_vregs() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let x = b.const_i32(42);
            let y = b.const_i32(10);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mut mf = select_function(&func, crate::target::TargetLayout::LP64);
        regalloc_naive(&mut mf);

        // After regalloc, no VReg operands should remain.
        for block in &mf.blocks {
            for inst in &block.insts {
                for op in &inst.operands {
                    assert!(
                        !matches!(op, MachineOperand::VReg(_)),
                        "vreg still present after regalloc: {:?}",
                        inst
                    );
                }
            }
        }
    }

    #[test]
    fn regalloc_frame_grows() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            b.const_i32(1);
            b.const_i32(2);
            b.const_i32(3);
            b.ret_void();
        }
        let mut mf = select_function(&func, crate::target::TargetLayout::LP64);
        let before = mf.frame.size;
        regalloc_naive(&mut mf);
        assert!(
            mf.frame.size >= before,
            "frame should grow to accommodate spill slots"
        );
    }

    #[test]
    fn regalloc_reuses_slots_for_disjoint_intervals() {
        let mut mf = MachineFunction::new("test".into());
        for value in 0..128 {
            let vreg = mf.new_vreg(RegClass::Gp64);
            mf.blocks[0].insts.push(MachineInst {
                opcode: ArmOpcode::Movz,
                operands: vec![MachineOperand::VReg(vreg), MachineOperand::Imm(value)],
                def: Some(vreg),
            });
            mf.blocks[0].insts.push(MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(0)),
                    MachineOperand::VReg(vreg),
                ],
                def: None,
            });
        }

        regalloc_naive(&mut mf);

        assert_eq!(
            mf.frame.locals.len(),
            1,
            "all sequential spill values should share one frame slot"
        );
    }

    #[test]
    fn regalloc_does_not_reuse_slots_with_same_instruction_uses() {
        let mut mf = MachineFunction::new("test".into());
        let lhs = mf.new_vreg(RegClass::Gp64);
        let rhs = mf.new_vreg(RegClass::Gp64);
        let result = mf.new_vreg(RegClass::Gp64);
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Movz,
            operands: vec![MachineOperand::VReg(lhs), MachineOperand::Imm(19)],
            def: Some(lhs),
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Movz,
            operands: vec![MachineOperand::VReg(rhs), MachineOperand::Imm(23)],
            def: Some(rhs),
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![
                MachineOperand::VReg(result),
                MachineOperand::VReg(lhs),
                MachineOperand::VReg(rhs),
            ],
            def: Some(result),
        });

        regalloc_naive(&mut mf);

        assert_eq!(
            mf.frame.locals.len(),
            3,
            "inputs and output at one instruction must retain distinct spill slots"
        );
    }

    #[test]
    fn regalloc_uses_scratch_registers() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mut mf = select_function(&func, crate::target::TargetLayout::LP64);
        regalloc_naive(&mut mf);

        // Should use x9, x10, x11 as scratch registers.
        let uses_scratch = mf.blocks.iter().any(|b| {
            b.insts.iter().any(|i| {
                i.operands.iter().any(|op| {
                    matches!(
                        op,
                        MachineOperand::PhysReg(PhysReg::Gp(9))
                            | MachineOperand::PhysReg(PhysReg::Gp(10))
                            | MachineOperand::PhysReg(PhysReg::Gp(11))
                            | MachineOperand::PhysReg(PhysReg::Gp32(9))
                            | MachineOperand::PhysReg(PhysReg::Gp32(10))
                            | MachineOperand::PhysReg(PhysReg::Gp32(11))
                    )
                })
            })
        });
        assert!(
            uses_scratch,
            "should use scratch registers x9-x11 or w9-w11"
        );
    }

    #[test]
    fn regalloc_reloads_spilled_fmla_accumulator() {
        let mut mf = MachineFunction::new("test".into());
        let accumulator = mf.new_vreg(RegClass::V128);
        let lhs = mf.new_vreg(RegClass::V128);
        let rhs = mf.new_vreg(RegClass::V128);
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::FmlaV4S,
            operands: vec![
                MachineOperand::VReg(accumulator),
                MachineOperand::VReg(lhs),
                MachineOperand::VReg(rhs),
            ],
            def: Some(accumulator),
        });

        regalloc_naive(&mut mf);

        let fmla_index = mf.blocks[0]
            .insts
            .iter()
            .position(|inst| inst.opcode == ArmOpcode::FmlaV4S)
            .expect("FMLA must survive register allocation");
        let reloads = &mf.blocks[0].insts[..fmla_index];
        assert_eq!(
            reloads
                .iter()
                .filter(|inst| inst.opcode == ArmOpcode::LdrQ)
                .count(),
            3,
            "FMLA must reload its accumulator as well as both multiplicands"
        );
        assert_eq!(
            mf.blocks[0].insts[fmla_index].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Fp(16)),
                MachineOperand::PhysReg(PhysReg::Fp(17)),
                MachineOperand::PhysReg(PhysReg::Fp(18)),
            ],
            "the destructive destination must reuse its accumulator reload"
        );
        let accumulator_reload = &reloads[0];
        let accumulator_store = &mf.blocks[0].insts[fmla_index + 1];
        assert_eq!(accumulator_store.opcode, ArmOpcode::StrQ);
        assert_eq!(
            accumulator_store.operands.first(),
            Some(&MachineOperand::PhysReg(PhysReg::Fp(16))),
            "the updated accumulator must be stored from its reloaded register"
        );
        assert_eq!(
            accumulator_store.operands.get(2),
            accumulator_reload.operands.get(2),
            "the updated accumulator must return to its original spill slot"
        );
    }
}
