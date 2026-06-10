//! Two-address conversion (sprint x05, deliverable 2).
//!
//! Isel emits three-address MIR (`d = op a, b`); x86's destructive
//! arithmetic requires `def == operands[0]` for every opcode with
//! `tied_use() == Some(0)` — the printer asserts it. This pass runs
//! pre-regalloc and rewrites untied instructions to
//! `mov d, a; d = op d, b`, with two refinements:
//!
//! - Commutative opcodes swap operands when `b` already aliases `d`
//!   (`d = a + d` → `d = d + a`), avoiding the mov entirely.
//! - Non-commutative `d == b` aliasing (`d = a - d` at the vreg
//!   level) would have `mov d, a` destroy `b` before the op reads
//!   it; `b` is routed through a fresh temp vreg first. After the
//!   rewrite `d` and the temp are live simultaneously across the
//!   mov+op, so their intervals interfere and regalloc cannot
//!   reintroduce the register-level alias.

use super::mir::{OpSize, X86Function, X86Inst, X86Opcode, X86Operand};

/// Tied opcodes whose operands may swap (`a op b == b op a`).
fn is_commutative(op: X86Opcode) -> bool {
    use X86Opcode::*;
    matches!(
        op,
        Add | Imul | And | Or | Xor | Addss | Addsd | Mulss | Mulsd | Xorps | Xorpd | Andps | Andpd
    )
}

/// Register-to-register move opcode matching the tied op's class.
/// Movsd for the f32 family is deliberate-by-omission-safe — the tie
/// copy only needs the low lane the scalar op reads — but Movss keeps
/// the printed asm width-honest.
fn move_opcode(op: X86Opcode) -> (X86Opcode, Option<OpSize>) {
    use X86Opcode::*;
    match op {
        Addss | Subss | Mulss | Divss | Xorps | Andps => (Movss, Some(OpSize::L)),
        Addsd | Subsd | Mulsd | Divsd | Xorpd | Andpd => (Movsd, Some(OpSize::Q)),
        // GP moves reuse the op's own size.
        _ => (MovRR, None),
    }
}

/// Rewrite every tied instruction so `def == operands[0]` holds.
pub fn convert_to_two_address(f: &mut X86Function) {
    for bi in 0..f.blocks.len() {
        let insts = std::mem::take(&mut f.blocks[bi].insts);
        let mut out: Vec<X86Inst> = Vec::with_capacity(insts.len());
        for mut inst in insts {
            let Some(tied) = inst.opcode.tied_use() else {
                out.push(inst);
                continue;
            };
            debug_assert_eq!(tied, 0, "x86 ties are all to operand 0");
            let def = inst
                .def
                .clone()
                .unwrap_or_else(|| panic!("{:?} is destructive but has no def", inst.opcode));
            if inst.operands[0] == def {
                out.push(inst);
                continue;
            }
            if is_commutative(inst.opcode) && inst.operands.get(1) == Some(&def) {
                inst.operands.swap(0, 1);
                out.push(inst);
                continue;
            }

            let (mv, mv_size) = move_opcode(inst.opcode);
            let mv_size = mv_size.unwrap_or(inst.size);

            if inst.operands.get(1) == Some(&def) {
                // The d==b pitfall: route b through a fresh temp.
                let class = match &def {
                    X86Operand::VReg(v) => v.class,
                    other => panic!(
                        "twoaddr: tied def aliasing operand 1 must be a vreg pre-regalloc, got {:?}",
                        other
                    ),
                };
                let temp = f.new_vreg(class);
                out.push(X86Inst {
                    opcode: mv,
                    size: mv_size,
                    operands: vec![inst.operands[1].clone()],
                    def: Some(X86Operand::VReg(temp)),
                });
                inst.operands[1] = X86Operand::VReg(temp);
            }

            out.push(X86Inst {
                opcode: mv,
                size: mv_size,
                operands: vec![inst.operands[0].clone()],
                def: Some(def.clone()),
            });
            inst.operands[0] = def;
            out.push(inst);
        }
        f.blocks[bi].insts = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::x86::mir::{X86RegClass, X86VReg};

    fn vreg_fn() -> (X86Function, X86VReg, X86VReg, X86VReg) {
        let mut f = X86Function::new("t".into());
        let a = f.new_vreg(X86RegClass::Gp32);
        let b = f.new_vreg(X86RegClass::Gp32);
        let d = f.new_vreg(X86RegClass::Gp32);
        (f, a, b, d)
    }

    fn inst(opcode: X86Opcode, operands: Vec<X86Operand>, def: X86VReg) -> X86Inst {
        X86Inst {
            opcode,
            size: OpSize::L,
            operands,
            def: Some(X86Operand::VReg(def)),
        }
    }

    #[test]
    fn tied_rewrite_inserts_mov() {
        let (mut f, a, b, d) = vreg_fn();
        f.blocks[0].insts.push(inst(
            X86Opcode::Sub,
            vec![X86Operand::VReg(a), X86Operand::VReg(b)],
            d,
        ));
        convert_to_two_address(&mut f);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 2);
        assert_eq!(insts[0].opcode, X86Opcode::MovRR);
        assert_eq!(insts[0].operands[0], X86Operand::VReg(a));
        assert_eq!(insts[0].def, Some(X86Operand::VReg(d)));
        assert_eq!(insts[1].opcode, X86Opcode::Sub);
        assert_eq!(insts[1].operands[0], X86Operand::VReg(d)); // tie holds
        assert_eq!(insts[1].operands[1], X86Operand::VReg(b));
    }

    #[test]
    fn commutative_swap_avoids_mov() {
        let (mut f, a, _b, d) = vreg_fn();
        // d = a + d → d = d + a, no copy needed.
        f.blocks[0].insts.push(inst(
            X86Opcode::Add,
            vec![X86Operand::VReg(a), X86Operand::VReg(d)],
            d,
        ));
        convert_to_two_address(&mut f);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 1);
        assert_eq!(insts[0].operands[0], X86Operand::VReg(d));
        assert_eq!(insts[0].operands[1], X86Operand::VReg(a));
    }

    #[test]
    fn d_aliases_b_copies_through_temp() {
        let (mut f, a, _b, d) = vreg_fn();
        // d = a - d: `mov d, a` first would destroy the subtrahend.
        f.blocks[0].insts.push(inst(
            X86Opcode::Sub,
            vec![X86Operand::VReg(a), X86Operand::VReg(d)],
            d,
        ));
        convert_to_two_address(&mut f);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 3);
        // mov temp, d
        assert_eq!(insts[0].opcode, X86Opcode::MovRR);
        assert_eq!(insts[0].operands[0], X86Operand::VReg(d));
        let temp = match &insts[0].def {
            Some(X86Operand::VReg(v)) => *v,
            other => panic!("expected vreg temp, got {:?}", other),
        };
        assert_ne!(temp, d, "temp must be fresh");
        // mov d, a
        assert_eq!(insts[1].opcode, X86Opcode::MovRR);
        assert_eq!(insts[1].operands[0], X86Operand::VReg(a));
        assert_eq!(insts[1].def, Some(X86Operand::VReg(d)));
        // sub temp from d
        assert_eq!(insts[2].opcode, X86Opcode::Sub);
        assert_eq!(insts[2].operands[0], X86Operand::VReg(d));
        assert_eq!(insts[2].operands[1], X86Operand::VReg(temp));
    }

    #[test]
    fn already_tied_left_alone() {
        let (mut f, a, _b, d) = vreg_fn();
        f.blocks[0].insts.push(inst(
            X86Opcode::Add,
            vec![X86Operand::VReg(d), X86Operand::VReg(a)],
            d,
        ));
        convert_to_two_address(&mut f);
        assert_eq!(f.blocks[0].insts.len(), 1);
    }

    #[test]
    fn sse_tie_uses_fp_move() {
        let mut f = X86Function::new("t".into());
        let a = f.new_vreg(X86RegClass::Xmm);
        let b = f.new_vreg(X86RegClass::Xmm);
        let d = f.new_vreg(X86RegClass::Xmm);
        f.blocks[0].insts.push(X86Inst {
            opcode: X86Opcode::Subsd,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(a), X86Operand::VReg(b)],
            def: Some(X86Operand::VReg(d)),
        });
        convert_to_two_address(&mut f);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 2);
        assert_eq!(insts[0].opcode, X86Opcode::Movsd);
        assert_eq!(insts[1].operands[0], X86Operand::VReg(d));
    }

    #[test]
    fn single_operand_tied_op_gets_mov() {
        let mut f = X86Function::new("t".into());
        let a = f.new_vreg(X86RegClass::Gp32);
        let d = f.new_vreg(X86RegClass::Gp32);
        f.blocks[0].insts.push(X86Inst {
            opcode: X86Opcode::Neg,
            size: OpSize::L,
            operands: vec![X86Operand::VReg(a)],
            def: Some(X86Operand::VReg(d)),
        });
        convert_to_two_address(&mut f);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 2);
        assert_eq!(insts[0].opcode, X86Opcode::MovRR);
        assert_eq!(insts[1].opcode, X86Opcode::Neg);
        assert_eq!(insts[1].operands[0], X86Operand::VReg(d));
    }
}
