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

/// Allocate registers for a machine function using spill-everything strategy.
/// Modifies the function in place: replaces VReg operands with PhysReg,
/// inserts loads/stores around each instruction.
pub fn regalloc_naive(mf: &mut MachineFunction) {
    // Phase 1: assign a stack slot to every vreg.
    let mut vreg_slots: HashMap<VRegId, i32> = HashMap::new();
    for vreg in &mf.vregs {
        let size = match vreg.class {
            RegClass::Gp64 => 8,
            RegClass::Gp32 => 4,
            RegClass::Fp64 => 8,
            RegClass::Fp32 => 4,
        };
        let offset = mf.frame.alloc_local(size);
        vreg_slots.insert(vreg.id, offset);
    }

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
                    // If this operand is the same as the def and it's the first
                    // operand (destination), it's an output — don't load.
                    if Some(*vid) == def_vreg && i == 0 {
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
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(42);
            let y = b.const_i32(10);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mut mf = select_function(&func);
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
            let mut b = FuncBuilder::new(&mut func);
            b.const_i32(1);
            b.const_i32(2);
            b.const_i32(3);
            b.ret_void();
        }
        let mut mf = select_function(&func);
        let before = mf.frame.size;
        regalloc_naive(&mut mf);
        assert!(
            mf.frame.size >= before,
            "frame should grow to accommodate spill slots"
        );
    }

    #[test]
    fn regalloc_uses_scratch_registers() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mut mf = select_function(&func);
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
}
