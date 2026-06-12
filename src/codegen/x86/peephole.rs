//! x86 backend peephole (sprint x09), run at O2+ on vreg MIR right
//! after instruction selection — the same driver gate as
//! `arm64::peephole`, before two-address conversion and regalloc.
//!
//! Patterns:
//!
//! * **cmp-zero → test**: `MovRI v, $0` + `Cmp(lhs, v)` becomes
//!   `Test(lhs, lhs)`, deleting the MovRI when `v` has no other use.
//!   `cmp $0, r` and `test r, r` produce identical RFLAGS (SF/ZF from
//!   the value, CF=OF=0 both ways), so every consumer condition is
//!   preserved. Only the rhs-zero form rewrites: for `Cmp(v, rhs)`
//!   with v=0 the conditions read `0 <op> rhs`, which `test` does not
//!   model (0 - INT_MIN overflows; r & r never sets OF).
//! * **self-move elimination**: `MovRR v, v` is a no-op.
//!
//! Deliberately absent (recorded in `.docs/audits/x09-pass-audit.md`):
//!
//! * `xor r, r` zeroing — writes RFLAGS, so it cannot be inserted
//!   blindly near the i128 Add/Adc and cmp/jcc adjacency the backend
//!   relies on; on naive-allocator slot traffic it also pessimizes.
//!   Revisit with the linear-scan allocator (x10).
//! * `lea` folding — address arithmetic is invisible under the naive
//!   allocator's load/op/store traffic; same x10 slot.
//! * inc/dec policy: the MIR has no Inc/Dec opcodes — isel always
//!   emits Add/Sub with an immediate, which is the documented choice
//!   (inc/dec leave CF unmodified and stall on partial-flag merges).
//! * No FMA contraction: SSE2 baseline has no fused multiply-add, and
//!   contraction changes last-ulp results (see the audit's FMA policy).

use std::collections::HashMap;

use super::mir::{X86Function, X86Opcode, X86Operand};
use crate::codegen::shared::VRegId;

/// Run all peephole patterns over one function. Returns the number of
/// instructions rewritten or removed (tests assert on it; the driver
/// ignores it).
pub fn run_peephole(f: &mut X86Function) -> usize {
    let mut changed = 0;
    changed += cmp_zero_to_test(f);
    changed += drop_self_moves(f);
    changed
}

/// Count uses of every vreg in operand position (defs excluded).
fn use_counts(f: &X86Function) -> HashMap<VRegId, usize> {
    let mut counts: HashMap<VRegId, usize> = HashMap::new();
    for block in &f.blocks {
        for inst in &block.insts {
            for op in &inst.operands {
                if let X86Operand::VReg(v) = op {
                    *counts.entry(v.id).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

fn cmp_zero_to_test(f: &mut X86Function) -> usize {
    let uses = use_counts(f);

    // The const may be materialized far from the Cmp (SSA constants
    // select once, often in the entry block), so block-local tracking
    // misses most sites. A vreg is a usable zero when its ONLY def in
    // the whole function is `MovRI v, $0` — multi-def vregs (block
    // param copies) are skipped.
    let mut def_counts: HashMap<VRegId, usize> = HashMap::new();
    let mut zero_site: HashMap<VRegId, (usize, usize)> = HashMap::new();
    for (bi, block) in f.blocks.iter().enumerate() {
        for (ii, inst) in block.insts.iter().enumerate() {
            if let Some(X86Operand::VReg(d)) = &inst.def {
                *def_counts.entry(d.id).or_insert(0) += 1;
                if inst.opcode == X86Opcode::MovRI
                    && matches!(inst.operands.first(), Some(X86Operand::Imm(0)))
                {
                    zero_site.insert(d.id, (bi, ii));
                }
            }
        }
    }
    let is_zero = |id: VRegId| def_counts.get(&id) == Some(&1) && zero_site.contains_key(&id);

    let mut changed = 0;
    let mut rewritten_uses: HashMap<VRegId, usize> = HashMap::new();
    for block in &mut f.blocks {
        for inst in &mut block.insts {
            if inst.opcode != X86Opcode::Cmp {
                continue;
            }
            let (Some(X86Operand::VReg(lhs)), Some(X86Operand::VReg(rhs))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            let (lhs, rhs) = (*lhs, *rhs);
            if !is_zero(rhs.id) {
                continue;
            }
            inst.opcode = X86Opcode::Test;
            inst.operands = vec![X86Operand::VReg(lhs), X86Operand::VReg(lhs)];
            *rewritten_uses.entry(rhs.id).or_insert(0) += 1;
            changed += 1;
        }
    }

    // Delete a zero's MovRI only when every use was rewritten away.
    let mut dead: Vec<(usize, usize)> = zero_site
        .iter()
        .filter(|(id, _)| rewritten_uses.get(id) == uses.get(id) && rewritten_uses.contains_key(id))
        .map(|(_, site)| *site)
        .collect();
    dead.sort_unstable();
    for &(bi, ii) in dead.iter().rev() {
        f.blocks[bi].insts.remove(ii);
    }
    changed
}

fn drop_self_moves(f: &mut X86Function) -> usize {
    let mut changed = 0;
    for block in &mut f.blocks {
        let before = block.insts.len();
        block.insts.retain(|inst| {
            if inst.opcode != X86Opcode::MovRR {
                return true;
            }
            match (&inst.def, inst.operands.first()) {
                (Some(X86Operand::VReg(d)), Some(X86Operand::VReg(s))) => d.id != s.id,
                (Some(X86Operand::Reg(d)), Some(X86Operand::Reg(s))) => d != s,
                _ => true,
            }
        });
        changed += before - block.insts.len();
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::shared::MBlockId;
    use crate::codegen::shared::VRegId;
    use crate::codegen::x86::mir::{OpSize, X86Block, X86Cond, X86Inst, X86RegClass, X86VReg};

    fn vreg(id: u32) -> X86VReg {
        X86VReg {
            id: VRegId(id),
            class: X86RegClass::Gp64,
        }
    }

    fn func_with(insts: Vec<X86Inst>) -> X86Function {
        let mut f = X86Function::new("t".into());
        f.blocks = vec![X86Block {
            id: MBlockId(0),
            insts,
        }];
        f
    }

    fn movri(dst: u32, imm: i64) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::MovRI,
            size: OpSize::Q,
            operands: vec![X86Operand::Imm(imm)],
            def: Some(X86Operand::VReg(vreg(dst))),
        }
    }

    fn cmp(a: u32, b: u32) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Cmp,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(vreg(a)), X86Operand::VReg(vreg(b))],
            def: None,
        }
    }

    fn setcc(cond: X86Cond, dst: u32) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Setcc,
            size: OpSize::B,
            operands: vec![X86Operand::Cond(cond)],
            def: Some(X86Operand::VReg(vreg(dst))),
        }
    }

    #[test]
    fn cmp_with_zero_rhs_becomes_test_and_movri_dies() {
        let mut f = func_with(vec![movri(1, 0), cmp(0, 1), setcc(X86Cond::E, 2)]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 1);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 2, "MovRI should be deleted: {:?}", insts);
        assert_eq!(insts[0].opcode, X86Opcode::Test);
        assert_eq!(
            insts[0].operands,
            vec![X86Operand::VReg(vreg(0)), X86Operand::VReg(vreg(0))]
        );
    }

    #[test]
    fn cmp_with_zero_lhs_is_left_alone() {
        // 0 <op> rhs is not test-representable (see module doc).
        let mut f = func_with(vec![movri(1, 0), cmp(1, 0), setcc(X86Cond::L, 2)]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 0);
        assert_eq!(f.blocks[0].insts[1].opcode, X86Opcode::Cmp);
    }

    #[test]
    fn multi_use_zero_rewrites_all_and_movri_dies() {
        let mut f = func_with(vec![
            movri(1, 0),
            cmp(0, 1),
            cmp(2, 1),
            setcc(X86Cond::E, 3),
        ]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 2, "both cmps rewrite");
        let insts = &f.blocks[0].insts;
        assert_eq!(
            insts.len(),
            3,
            "all uses rewritten, MovRI dies: {:?}",
            insts
        );
        assert_eq!(insts[0].opcode, X86Opcode::Test);
        assert_eq!(insts[1].opcode, X86Opcode::Test);
    }

    #[test]
    fn zero_with_surviving_non_cmp_use_keeps_the_movri() {
        // The zero also feeds an Add — only the Cmp use rewrites, so
        // the materialization must stay.
        let add = X86Inst {
            opcode: X86Opcode::Add,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(vreg(2)), X86Operand::VReg(vreg(1))],
            def: Some(X86Operand::VReg(vreg(2))),
        };
        let mut f = func_with(vec![movri(1, 0), cmp(0, 1), add]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 1);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 3, "MovRI must survive: {:?}", insts);
        assert_eq!(insts[0].opcode, X86Opcode::MovRI);
        assert_eq!(insts[1].opcode, X86Opcode::Test);
    }

    #[test]
    fn redefined_vreg_is_not_treated_as_zero() {
        let mut f = func_with(vec![movri(1, 0), movri(1, 7), cmp(0, 1)]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 0);
        assert_eq!(f.blocks[0].insts[2].opcode, X86Opcode::Cmp);
    }

    #[test]
    fn nonzero_immediate_is_not_rewritten() {
        let mut f = func_with(vec![movri(1, 5), cmp(0, 1)]);
        assert_eq!(run_peephole(&mut f), 0);
    }

    #[test]
    fn self_move_dropped() {
        let mut f = func_with(vec![X86Inst {
            opcode: X86Opcode::MovRR,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(vreg(3))],
            def: Some(X86Operand::VReg(vreg(3))),
        }]);
        assert_eq!(run_peephole(&mut f), 1);
        assert!(f.blocks[0].insts.is_empty());
    }
}
