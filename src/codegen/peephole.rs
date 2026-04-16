//! Post-isel peephole optimizations.
//!
//! Runs on `MachineFunction` after instruction selection and before
//! register allocation. Virtual registers are still in use.
//!
//! ## FMADD / FMSUB fusion
//!
//! ARM64 has fused multiply-add instructions that are both faster and
//! more accurate (single rounding) than a separate fmul + fadd pair.
//!
//! Matched patterns and their replacement:
//!
//! | Pattern                               | Replacement                 | ARM64 opcode |
//! |---------------------------------------|-----------------------------|--------------|
//! | `fadd(fmul(a, b), c)` or commuted    | `result = c + a*b`          | FMADD        |
//! | `fsub(c, fmul(a, b))`                | `result = c - a*b`          | FMSUB        |
//! | `fsub(fmul(a, b), c)`                | `result = a*b - c`          | FNMSUB       |
//!
//! Preconditions:
//! * The FMUL result is used exactly once (the subsequent FADD/FSUB).
//! * Both instructions are in the same machine block.
//!
//! After fusion the FMUL instruction is removed and the FADD/FSUB is
//! replaced by the three-source FMA instruction.

use super::mir::{ArmOpcode, MachineFunction, MachineInst, MachineOperand, VRegId};
use std::collections::HashMap;

/// Run all peephole passes on a machine function.
pub fn run_peephole(mf: &mut MachineFunction) {
    fma_fusion(mf);
}

/// FMADD/FMSUB/FNMSUB fusion.
fn fma_fusion(mf: &mut MachineFunction) {
    for mb_idx in 0..mf.blocks.len() {
        fma_fuse_block(mf, mb_idx);
    }
}

fn fma_fuse_block(mf: &mut MachineFunction, mb_idx: usize) {
    let block = &mf.blocks[mb_idx];

    // Count uses of each defined VReg within this block.
    // A fmul result is fusable only when its use count == 1 (the fadd/fsub).
    let mut use_count: HashMap<VRegId, usize> = HashMap::new();
    for inst in &block.insts {
        for op in &inst.operands {
            if let MachineOperand::VReg(v) = op {
                *use_count.entry(*v).or_insert(0) += 1;
            }
        }
    }
    // Subtract the self-def (operands[0] is the dest, counted in use_count
    // but it's a def not a use). For a 3-operand FmulS [dest, src0, src1]:
    // dest appears once in operands but it's the definition, not a use.
    // Re-compute: only operands[1..] are uses.
    let mut use_count: HashMap<VRegId, usize> = HashMap::new();
    for inst in &block.insts {
        // Operands beyond index 0 are inputs (index 0 is the output dest).
        for op in inst.operands.iter().skip(1) {
            if let MachineOperand::VReg(v) = op {
                *use_count.entry(*v).or_insert(0) += 1;
            }
        }
    }

    // Map: vreg defined by a fmul instruction → (block-instruction-index, precision)
    #[derive(Clone, Copy)]
    enum Prec {
        S,
        D,
    }
    let mut fmul_defs: HashMap<VRegId, (usize, Prec)> = HashMap::new();

    for (i, inst) in block.insts.iter().enumerate() {
        match inst.opcode {
            ArmOpcode::FmulS => {
                if let Some(d) = inst.def {
                    fmul_defs.insert(d, (i, Prec::S));
                }
            }
            ArmOpcode::FmulD => {
                if let Some(d) = inst.def {
                    fmul_defs.insert(d, (i, Prec::D));
                }
            }
            _ => {}
        }
    }

    // Collect fusions to apply (instruction index → replacement info).
    // Each entry: (fadd/fsub_idx, fmul_idx, new_opcode, new_operands)
    struct FusionPlan {
        add_idx: usize,
        mul_idx: usize,
        new_opcode: ArmOpcode,
        // operands for the fused inst: [dest, Fn, Fm, Fa]
        n: VRegId, // multiply lhs
        m: VRegId, // multiply rhs
        a: VRegId, // addend / subtrahend (accumulate register)
    }
    let mut plans: Vec<FusionPlan> = Vec::new();

    let block = &mf.blocks[mb_idx];
    for (i, inst) in block.insts.iter().enumerate() {
        let (is_add, is_sub, prec_s) = match inst.opcode {
            ArmOpcode::FaddS => (true, false, true),
            ArmOpcode::FaddD => (true, false, false),
            ArmOpcode::FsubS => (false, true, true),
            ArmOpcode::FsubD => (false, true, false),
            _ => continue,
        };
        if inst.operands.len() < 3 {
            continue;
        }

        // operands: [dest, src0, src1]
        let src0 = match &inst.operands[1] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };
        let src1 = match &inst.operands[2] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };

        // Check if src0 is a single-use fmul result.
        let try_fuse_src0 = fmul_defs
            .get(&src0)
            .filter(|_| use_count.get(&src0).copied().unwrap_or(0) == 1);
        // Check if src1 is a single-use fmul result.
        let try_fuse_src1 = if is_add {
            fmul_defs
                .get(&src1)
                .filter(|_| use_count.get(&src1).copied().unwrap_or(0) == 1)
        } else {
            None // fsub(c, fmul(a,b)): src0=c, src1=fmul → handled separately
        };

        if is_add {
            // fadd(fmul(a,b), c) → FMADD(a, b, c)
            if let Some(&(mul_idx, _)) = try_fuse_src0 {
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FmaddS
                } else {
                    ArmOpcode::FmaddD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src1,
                });
            } else if let Some(&(mul_idx, _)) = try_fuse_src1 {
                // fadd(c, fmul(a,b)) → FMADD(a, b, c)  [commuted]
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FmaddS
                } else {
                    ArmOpcode::FmaddD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src0,
                });
            }
        } else if is_sub {
            // fsub(fmul(a,b), c) → FNMSUB(a, b, c)  [result = a*b - c]
            if let Some(&(mul_idx, _)) = try_fuse_src0 {
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FnmsubS
                } else {
                    ArmOpcode::FnmsubD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src1,
                });
            }
            // fsub(c, fmul(a,b)) → FMSUB(a, b, c)  [result = c - a*b]
            // src0=c, src1=fmul_result
            let try_fuse_sub1 = fmul_defs
                .get(&src1)
                .filter(|_| use_count.get(&src1).copied().unwrap_or(0) == 1);
            if let Some(&(mul_idx, _)) = try_fuse_sub1 {
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FmsubS
                } else {
                    ArmOpcode::FmsubD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src0,
                });
            }
        }
    }

    if plans.is_empty() {
        return;
    }

    // Apply plans in reverse order of add_idx to keep indices stable.
    plans.sort_by_key(|plan| std::cmp::Reverse(plan.add_idx));

    // Collect mul_idxs to remove.
    let mut remove_idxs: std::collections::HashSet<usize> =
        plans.iter().map(|p| p.mul_idx).collect();

    // Rewrite the block.
    let block = &mut mf.blocks[mb_idx];
    for plan in &plans {
        let dest = block.insts[plan.add_idx].def;
        let dest_op = block.insts[plan.add_idx].operands[0].clone();
        block.insts[plan.add_idx] = MachineInst {
            opcode: plan.new_opcode,
            operands: vec![
                dest_op,
                MachineOperand::VReg(plan.n),
                MachineOperand::VReg(plan.m),
                MachineOperand::VReg(plan.a),
            ],
            def: dest,
        };
    }
    let mut idx = 0usize;
    block.insts.retain(|_| {
        let keep = !remove_idxs.remove(&idx);
        idx += 1;
        keep
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::mir::{
        ArmOpcode, MBlockId, MachineBlock, MachineFunction, MachineInst, MachineOperand, RegClass,
    };

    fn mf_with_insts(insts: Vec<MachineInst>) -> MachineFunction {
        let mut mf = MachineFunction::new("test".into());
        // Allocate enough vregs.
        for _ in 0..10 {
            mf.new_vreg(RegClass::Fp32);
        }
        let bid = MBlockId(0);
        mf.blocks = vec![MachineBlock {
            id: bid,
            label: "entry".into(),
            insts,
        }];
        mf
    }

    fn vreg(v: u32) -> MachineOperand {
        MachineOperand::VReg(VRegId(v))
    }
    fn vid(v: u32) -> VRegId {
        VRegId(v)
    }

    /// fadd(fmul(v1, v2), v3) → fmadd(v1, v2, v3)
    #[test]
    fn fmadd_f32() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulS,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fadd = MachineInst {
            opcode: ArmOpcode::FaddS,
            operands: vec![vreg(4), vreg(0), vreg(3)], // src0 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fadd]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1, "fmul should be removed");
        assert_eq!(block.insts[0].opcode, ArmOpcode::FmaddS);
        // operands: [dest, n, m, a] = [v4, v1, v2, v3]
        assert_eq!(block.insts[0].operands[1], vreg(1));
        assert_eq!(block.insts[0].operands[2], vreg(2));
        assert_eq!(block.insts[0].operands[3], vreg(3));
    }

    /// fadd(v3, fmul(v1, v2)) → fmadd(v1, v2, v3)  [commuted]
    #[test]
    fn fmadd_commuted() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulD,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fadd = MachineInst {
            opcode: ArmOpcode::FaddD,
            operands: vec![vreg(4), vreg(3), vreg(0)], // src1 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fadd]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::FmaddD);
        assert_eq!(block.insts[0].operands[3], vreg(3)); // accumulator
    }

    /// fsub(c, fmul(a,b)) → fmsub(a, b, c)  [result = c - a*b]
    #[test]
    fn fmsub_f32() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulS,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fsub = MachineInst {
            opcode: ArmOpcode::FsubS,
            operands: vec![vreg(4), vreg(3), vreg(0)], // src1 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fsub]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::FmsubS);
        assert_eq!(block.insts[0].operands[3], vreg(3)); // Sa = c
    }

    /// fsub(fmul(a,b), c) → fnmsub(a, b, c)  [result = a*b - c]
    #[test]
    fn fnmsub_f64() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulD,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fsub = MachineInst {
            opcode: ArmOpcode::FsubD,
            operands: vec![vreg(4), vreg(0), vreg(3)], // src0 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fsub]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::FnmsubD);
    }

    /// fmul result used twice: must NOT be fused.
    #[test]
    fn no_fusion_if_mul_used_twice() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulS,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        // Two consumers of vreg(0):
        let fadd1 = MachineInst {
            opcode: ArmOpcode::FaddS,
            operands: vec![vreg(4), vreg(0), vreg(3)],
            def: Some(vid(4)),
        };
        let fadd2 = MachineInst {
            opcode: ArmOpcode::FaddS,
            operands: vec![vreg(5), vreg(0), vreg(3)],
            def: Some(vid(5)),
        };
        let mut mf = mf_with_insts(vec![fmul, fadd1, fadd2]);
        fma_fusion(&mut mf);
        // No fusion — fmul has 2 uses.
        assert_eq!(mf.blocks[0].insts.len(), 3);
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::FmulS);
    }
}
