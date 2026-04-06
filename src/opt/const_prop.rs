//! Constant propagation pass.
//!
//! In our SSA IR, propagating a constant value into the operand slot of
//! an instruction is a no-op rewrite — every consumer already finds the
//! defining `Const*` instruction by `ValueId`, and codegen happily turns
//! that into an immediate. The transformations that *do* matter are the
//! ones the constant-folding pass cannot reach because they live in
//! block terminators:
//!
//!  * `CondBranch { cond, true_dest, false_dest }` with a constant
//!    `cond` becomes an unconditional `Branch` to the live target.
//!  * `Switch { selector, cases, default }` with a constant `selector`
//!    becomes an unconditional `Branch` to the matching case.
//!
//! Both rewrites are critical: they expose dead blocks to later passes
//! (CSE, LICM, GVN) and shrink the CFG.
//!
//! After folding terminators we **must** prune any blocks that are no
//! longer reachable from the entry. Leaving an orphan reachable through
//! some other path could be fine, but our verifier rejects values used
//! across blocks that no longer dominate the use — and unreachable
//! blocks can break dominance for downstream merges. The cleanest fix
//! is to remove the dead blocks as part of the same transformation so
//! the IR handed to the verifier is always well-formed.
//!
//! The pass is conservative: if the picked target has block parameters
//! (which Fortran lowering only emits for loop headers and merges), we
//! still rewrite — we just keep the original branch arguments for the
//! taken side, since that side already had matching args in the
//! original `CondBranch`.

use super::pass::Pass;
use super::util::prune_unreachable;
use crate::ir::inst::*;
use crate::ir::types::IntWidth;
use std::collections::HashMap;

/// What we know about a value at compile time.
#[derive(Debug, Clone, Copy)]
enum Const {
    Bool(bool),
    Int(i64, IntWidth),
}

impl Const {
    fn from_inst(kind: &InstKind) -> Option<Self> {
        match kind {
            InstKind::ConstBool(b) => Some(Const::Bool(*b)),
            InstKind::ConstInt(v, w) => Some(Const::Int(*v, *w)),
            _ => None,
        }
    }
}

/// Build the const-value map for one function. Walks every block in
/// definition order; SSA dominance guarantees an instruction's operands
/// are already in the map when we reach it.
fn collect_consts(func: &Function) -> HashMap<ValueId, Const> {
    let mut consts = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(c) = Const::from_inst(&inst.kind) {
                consts.insert(inst.id, c);
            }
        }
    }
    consts
}

/// The constant propagation pass.
pub struct ConstProp;

impl Pass for ConstProp {
    fn name(&self) -> &'static str { "const-prop" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            let consts = collect_consts(func);
            let mut local_changed = false;
            for block in &mut func.blocks {
                let Some(term) = block.terminator.take() else { continue };
                let new_term = simplify_terminator(term, &consts, &mut local_changed);
                block.terminator = Some(new_term);
            }
            if local_changed {
                // Folded a CondBranch/Switch — drop any block that is no
                // longer reachable from the entry so the verifier doesn't
                // see stale uses across a now-impossible path.
                let _pruned = prune_unreachable(func);
                changed = true;
            }
        }
        changed
    }
}

fn simplify_terminator(
    term: Terminator,
    consts: &HashMap<ValueId, Const>,
    changed: &mut bool,
) -> Terminator {
    match term {
        Terminator::CondBranch { cond, true_dest, true_args, false_dest, false_args } => {
            if let Some(Const::Bool(c)) = consts.get(&cond).copied() {
                *changed = true;
                if c {
                    Terminator::Branch(true_dest, true_args)
                } else {
                    Terminator::Branch(false_dest, false_args)
                }
            } else {
                Terminator::CondBranch { cond, true_dest, true_args, false_dest, false_args }
            }
        }
        Terminator::Switch { selector, cases, default } => {
            if let Some(Const::Int(v, w)) = consts.get(&selector).copied() {
                // Sign-extend stored i64 to true value at the operand width.
                let sv = sext(v, w.bits());
                *changed = true;
                let target = cases.iter().find(|(k, _)| sext(*k, w.bits()) == sv)
                    .map(|(_, b)| *b)
                    .unwrap_or(default);
                // Switch targets in our IR cannot have block params, so
                // an empty arg vec is correct.
                Terminator::Branch(target, vec![])
            } else {
                Terminator::Switch { selector, cases, default }
            }
        }
        other => other,
    }
}

fn sext(v: i64, bits: u32) -> i64 {
    if bits >= 64 { v }
    else {
        let shift = 64 - bits;
        (v << shift) >> shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::lexer::{Span, Position};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span { start: p, end: p, file_id: 0 }
    }

    #[test]
    fn condbranch_with_true_const_folds_to_branch_true() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        // Build: entry { cond=const(true); cond_branch cond, then, else }
        //        then  { ret }
        //        else  { ret }
        let bb_t = f.create_block("then");
        let bb_e = f.create_block("else");
        let cond_id = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: cond_id,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: cond_id,
            true_dest: bb_t,
            true_args: vec![],
            false_dest: bb_e,
            false_args: vec![],
        });
        f.block_mut(bb_t).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_e).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(ConstProp.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Branch(dest, _)) => assert_eq!(*dest, bb_t),
            other => panic!("expected Branch, got {:?}", other),
        }
    }

    #[test]
    fn condbranch_with_false_const_folds_to_branch_false() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let bb_t = f.create_block("then");
        let bb_e = f.create_block("else");
        let cond_id = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: cond_id,
            kind: InstKind::ConstBool(false),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: cond_id,
            true_dest: bb_t,
            true_args: vec![],
            false_dest: bb_e,
            false_args: vec![],
        });
        f.block_mut(bb_t).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_e).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(ConstProp.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Branch(dest, _)) => assert_eq!(*dest, bb_e),
            other => panic!("expected Branch, got {:?}", other),
        }
    }

    #[test]
    fn switch_with_const_selector_picks_case() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let case_a = f.create_block("a");
        let case_b = f.create_block("b");
        let default = f.create_block("default");
        let sel_id = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: sel_id,
            kind: InstKind::ConstInt(2, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Switch {
            selector: sel_id,
            cases: vec![(1, case_a), (2, case_b)],
            default,
        });
        f.block_mut(case_a).terminator = Some(Terminator::Return(None));
        f.block_mut(case_b).terminator = Some(Terminator::Return(None));
        f.block_mut(default).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(ConstProp.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Branch(dest, _)) => assert_eq!(*dest, case_b),
            other => panic!("expected Branch, got {:?}", other),
        }
    }

    #[test]
    fn switch_with_unmatched_const_selector_picks_default() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let case_a = f.create_block("a");
        let default = f.create_block("default");
        let sel_id = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: sel_id,
            kind: InstKind::ConstInt(99, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Switch {
            selector: sel_id,
            cases: vec![(1, case_a)],
            default,
        });
        f.block_mut(case_a).terminator = Some(Terminator::Return(None));
        f.block_mut(default).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(ConstProp.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Branch(dest, _)) => assert_eq!(*dest, default),
            other => panic!("expected Branch, got {:?}", other),
        }
    }

    #[test]
    fn folded_condbranch_prunes_dead_block() {
        // entry { cond=const(true); cond_branch -> then or dead }
        // then { ret }
        // dead { ret }   // should be removed
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let bb_t = f.create_block("then");
        let bb_d = f.create_block("dead");
        let cond_id = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: cond_id,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: cond_id,
            true_dest: bb_t,
            true_args: vec![],
            false_dest: bb_d,
            false_args: vec![],
        });
        f.block_mut(bb_t).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_d).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(ConstProp.run(&mut m));
        let f = &m.functions[0];
        assert_eq!(f.blocks.len(), 2, "dead block should be pruned");
        assert!(f.blocks.iter().any(|b| b.id == bb_t));
        assert!(!f.blocks.iter().any(|b| b.id == bb_d));
    }

    #[test]
    fn condbranch_with_unknown_cond_left_alone() {
        // Operand is not a constant — must not transform.
        let mut m = Module::new("t".into());
        let params = vec![Param {
            name: "p".into(),
            ty: IrType::Bool,
            id: ValueId(0),
        }];
        let mut f = Function::new("f".into(), params, IrType::Void);
        let bb_t = f.create_block("then");
        let bb_e = f.create_block("else");
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: ValueId(0),
            true_dest: bb_t,
            true_args: vec![],
            false_dest: bb_e,
            false_args: vec![],
        });
        f.block_mut(bb_t).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_e).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!ConstProp.run(&mut m));
        assert!(matches!(
            m.functions[0].blocks[0].terminator,
            Some(Terminator::CondBranch { .. })
        ));
    }
}
