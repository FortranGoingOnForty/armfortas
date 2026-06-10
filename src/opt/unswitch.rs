//! Loop unswitching pass.
//!
//! Hoists loop-invariant conditionals out of loops by cloning the loop
//! into two versions — one for the true branch, one for the false
//! branch. Each clone has the internal conditional replaced with an
//! unconditional branch, eliminating the branch from the hot path.
//!
//! ```text
//! Before:
//!   do i = 1, n
//!     if (flag) then      ← flag is loop-invariant
//!       a(i) = b(i)
//!     else
//!       a(i) = c(i)
//!     end if
//!   end do
//!
//! After:
//!   if (flag) then
//!     do i = 1, n; a(i) = b(i); end do
//!   else
//!     do i = 1, n; a(i) = c(i); end do
//!   end if
//! ```
//!
//! Gated on body size (≤ UNSWITCH_MAX_BODY instructions) to prevent
//! code bloat. Fires at O2+.

use super::loop_utils::{find_preheader, loop_defined_values};
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::{
    find_natural_loops, inst_uses, predecessors, prune_unreachable, terminator_uses,
};
use std::collections::{HashMap, HashSet};

/// Maximum number of instructions in the loop body to consider for
/// unswitching. Unswitching doubles the code, so keep this tight.
const UNSWITCH_MAX_BODY: usize = 50;

pub struct LoopUnswitch;

type UnswitchCandidate = (
    BlockId,
    ValueId,
    BlockId,
    Vec<ValueId>,
    BlockId,
    Vec<ValueId>,
);

impl Pass for LoopUnswitch {
    fn name(&self) -> &'static str {
        "loop-unswitch"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if unswitch_in_function(func) {
                changed = true;
            }
        }
        changed
    }
}

/// Attempt to unswitch one loop in the function. Returns true if a
/// transformation was applied. We process one loop per call and let
/// the pass manager's fixpoint loop handle cascading opportunities.
fn unswitch_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);

    for lp in &loops {
        let Some(ph_id) = find_preheader(func, lp, &preds) else {
            continue;
        };

        // Size guard.
        let total_insts: usize = lp.body.iter().map(|&b| func.block(b).insts.len()).sum();
        if total_insts > UNSWITCH_MAX_BODY {
            continue;
        }

        // Find a CondBranch inside the loop whose condition is invariant.
        let loop_defs = loop_defined_values(func, lp);
        if has_external_ssa_uses(func, lp, &loop_defs) {
            continue;
        }

        let candidate = find_unswitch_candidate(func, lp, &loop_defs);
        let Some((cond_block, cond_val, true_dest, true_args, false_dest, false_args)) = candidate
        else {
            continue;
        };

        // Both successors must be inside the loop (otherwise it's the
        // loop exit condition, not an unswitchable interior branch).
        if !lp.body.contains(&true_dest) || !lp.body.contains(&false_dest) {
            continue;
        }

        // Clone the entire loop body into two copies.
        let (true_map, true_blocks) = clone_loop(func, lp);
        let (false_map, false_blocks) = clone_loop(func, lp);

        // In the true clone: replace the CondBranch with an unconditional
        // branch to the true successor.
        let true_cond_block = true_map[&cond_block];
        let true_true_dest = true_map[&true_dest];
        let true_val_map = build_value_map(func, lp, &true_blocks, &true_map);
        let remapped_true_args: Vec<ValueId> = true_args
            .iter()
            .map(|v| *true_val_map.get(v).unwrap_or(v))
            .collect();
        func.block_mut(true_cond_block).terminator =
            Some(Terminator::Branch(true_true_dest, remapped_true_args));

        // In the false clone: replace the CondBranch with an unconditional
        // branch to the false successor.
        let false_cond_block = false_map[&cond_block];
        let false_false_dest = false_map[&false_dest];
        let false_val_map = build_value_map(func, lp, &false_blocks, &false_map);
        let remapped_false_args: Vec<ValueId> = false_args
            .iter()
            .map(|v| *false_val_map.get(v).unwrap_or(v))
            .collect();
        func.block_mut(false_cond_block).terminator =
            Some(Terminator::Branch(false_false_dest, remapped_false_args));

        // Rewrite the preheader to test the condition and branch to
        // the appropriate clone's header.
        let true_header = true_map[&lp.header];
        let false_header = false_map[&lp.header];

        // Get the preheader's original branch args to the header.
        let ph_args = match &func.block(ph_id).terminator {
            Some(Terminator::Branch(_, args)) => args.clone(),
            _ => vec![],
        };

        func.block_mut(ph_id).terminator = Some(Terminator::CondBranch {
            cond: cond_val,
            true_dest: true_header,
            true_args: ph_args.clone(),
            false_dest: false_header,
            false_args: ph_args,
        });

        // Mark original loop blocks as unreachable so prune removes them.
        for &bid in &lp.body {
            func.block_mut(bid).terminator = Some(Terminator::Unreachable);
        }
        prune_unreachable(func);

        return true; // one at a time
    }
    false
}

/// Find a CondBranch in the loop body whose condition is loop-invariant.
fn find_unswitch_candidate(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    loop_defs: &HashSet<ValueId>,
) -> Option<UnswitchCandidate> {
    for &bid in &lp.body {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        }) = &block.terminator
        {
            // Condition must be loop-invariant (not defined in the loop).
            if !loop_defs.contains(cond) {
                // Both targets must be in the loop (not the loop exit).
                if lp.body.contains(true_dest) && lp.body.contains(false_dest) {
                    return Some((
                        bid,
                        *cond,
                        *true_dest,
                        true_args.clone(),
                        *false_dest,
                        false_args.clone(),
                    ));
                }
            }
        }
    }
    None
}

/// Unswitching clones the loop body and removes the original loop blocks.
/// If later blocks still read a loop-defined SSA value directly, the transform
/// would need extra SSA repair on the outside uses. Until that exists, bail out.
fn has_external_ssa_uses(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    loop_defs: &HashSet<ValueId>,
) -> bool {
    for block in &func.blocks {
        if lp.body.contains(&block.id) {
            continue;
        }
        for inst in &block.insts {
            if inst_uses(&inst.kind)
                .into_iter()
                .any(|value| loop_defs.contains(&value))
            {
                return true;
            }
        }
        if let Some(term) = &block.terminator {
            if terminator_uses(term)
                .into_iter()
                .any(|value| loop_defs.contains(&value))
            {
                return true;
            }
        }
    }
    false
}

/// Delegate to shared loop utility.
fn clone_loop(
    func: &mut Function,
    lp: &crate::ir::walk::NaturalLoop,
) -> (HashMap<BlockId, BlockId>, Vec<BlockId>) {
    super::loop_utils::clone_loop(func, lp)
}

/// Delegate to shared loop utility.
fn build_value_map(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    _new_blocks: &[BlockId],
    block_map: &HashMap<BlockId, BlockId>,
) -> HashMap<ValueId, ValueId> {
    super::loop_utils::build_value_map(func, lp, block_map)
}

// remap_inst_kind and remap_terminator have been moved to loop_utils.rs.
// The unswitching pass's clone_loop delegate above calls them transitively.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::ir::verify::verify_module;
    use crate::lexer::{Position, Span};
    use crate::opt::pass::Pass;

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    /// Build: entry → preheader → header(%i) → cmp → body → cond_branch(flag, t_body, f_body) →
    ///        t_body → latch; f_body → latch; latch → header
    ///
    /// `flag` is defined in entry (loop-invariant).
    fn build_unswitchable_loop() -> Module {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let preheader = f.create_block("preheader");
        let header = f.create_block("header");
        let cmp_blk = f.create_block("cmp");
        let body = f.create_block("body");
        let t_body = f.create_block("t_body");
        let f_body = f.create_block("f_body");
        let latch = f.create_block("latch");
        let exit = f.create_block("exit");
        let entry = f.entry;

        // Entry: flag = const_bool(true), c1 = const 1, c10 = const 10
        let flag = f.next_value_id();
        f.register_type(flag, IrType::Bool);
        f.block_mut(entry).insts.push(Inst {
            id: flag,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ConstBool(true),
        });
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let c10 = f.next_value_id();
        f.register_type(c10, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c10,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(preheader, vec![]));

        // Preheader → header(c1)
        f.block_mut(preheader).terminator = Some(Terminator::Branch(header, vec![c1]));

        // Header(%i) → cmp
        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(header).terminator = Some(Terminator::Branch(cmp_blk, vec![]));

        // Cmp: icmp le %i, 10; condBr body, exit
        let cmp_v = f.next_value_id();
        f.register_type(cmp_v, IrType::Bool);
        f.block_mut(cmp_blk).insts.push(Inst {
            id: cmp_v,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c10),
        });
        f.block_mut(cmp_blk).terminator = Some(Terminator::CondBranch {
            cond: cmp_v,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        // Body: condBr flag, t_body, f_body  ← the unswitchable conditional
        f.block_mut(body).terminator = Some(Terminator::CondBranch {
            cond: flag,
            true_dest: t_body,
            true_args: vec![],
            false_dest: f_body,
            false_args: vec![],
        });

        // t_body → latch
        f.block_mut(t_body).terminator = Some(Terminator::Branch(latch, vec![]));

        // f_body → latch
        f.block_mut(f_body).terminator = Some(Terminator::Branch(latch, vec![]));

        // Latch: iadd + br header
        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![nxt]));

        // Exit
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);
        m
    }

    #[test]
    fn unswitches_invariant_conditional() {
        let mut m = build_unswitchable_loop();
        let pass = LoopUnswitch;
        let changed = pass.run(&mut m);
        assert!(changed, "should unswitch the invariant conditional");

        // After unswitching, the preheader should have a CondBranch (not a Branch).
        let f = &m.functions[0];
        let preheader = f
            .blocks
            .iter()
            .find(|b| b.name.contains("preheader"))
            .unwrap();
        assert!(
            matches!(&preheader.terminator, Some(Terminator::CondBranch { .. })),
            "preheader should now have a CondBranch: {:?}",
            preheader.terminator
        );
    }

    #[test]
    fn does_not_unswitch_variant_conditional() {
        // Build a loop where the condition IS loop-defined (the IV).
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let header = f.create_block("header");
        let cmp_blk = f.create_block("cmp");
        let body = f.create_block("body");
        let latch = f.create_block("latch");
        let exit = f.create_block("exit");
        let entry = f.entry;

        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let c10 = f.next_value_id();
        f.register_type(c10, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c10,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![c1]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        let cmp_v = f.next_value_id();
        f.register_type(cmp_v, IrType::Bool);
        f.block_mut(header).insts.push(Inst {
            id: cmp_v,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c10),
        });
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp_v,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        // Body: the "conditional" uses the IV (loop-variant).
        let iv_cmp = f.next_value_id();
        f.register_type(iv_cmp, IrType::Bool);
        f.block_mut(body).insts.push(Inst {
            id: iv_cmp,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c1),
        });
        f.block_mut(body).terminator = Some(Terminator::CondBranch {
            cond: iv_cmp,
            true_dest: latch,
            true_args: vec![],
            false_dest: latch,
            false_args: vec![],
        });

        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![nxt]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);

        let pass = LoopUnswitch;
        let changed = pass.run(&mut m);
        assert!(!changed, "should not unswitch a loop-variant conditional");
    }

    #[test]
    fn does_not_unswitch_when_loop_values_escape_directly() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let preheader = f.create_block("preheader");
        let header = f.create_block("header");
        let body = f.create_block("body");
        let t_body = f.create_block("t_body");
        let f_body = f.create_block("f_body");
        let latch = f.create_block("latch");
        let exit = f.create_block("exit");
        let entry = f.entry;

        let flag = f.next_value_id();
        f.register_type(flag, IrType::Bool);
        f.block_mut(entry).insts.push(Inst {
            id: flag,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ConstBool(true),
        });
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let c10 = f.next_value_id();
        f.register_type(c10, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c10,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(preheader, vec![]));

        f.block_mut(preheader).terminator = Some(Terminator::Branch(header, vec![c1]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(header).terminator = Some(Terminator::Branch(body, vec![]));

        f.block_mut(body).terminator = Some(Terminator::CondBranch {
            cond: flag,
            true_dest: t_body,
            true_args: vec![],
            false_dest: f_body,
            false_args: vec![],
        });
        f.block_mut(t_body).terminator = Some(Terminator::Branch(latch, vec![]));
        f.block_mut(f_body).terminator = Some(Terminator::Branch(latch, vec![]));

        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        let cmp_v = f.next_value_id();
        f.register_type(cmp_v, IrType::Bool);
        f.block_mut(latch).insts.push(Inst {
            id: cmp_v,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, nxt, c10),
        });
        f.block_mut(latch).terminator = Some(Terminator::CondBranch {
            cond: cmp_v,
            true_dest: header,
            true_args: vec![nxt],
            false_dest: exit,
            false_args: vec![],
        });

        let escaped = f.next_value_id();
        f.register_type(escaped, IrType::Int(IntWidth::I32));
        f.block_mut(exit).insts.push(Inst {
            id: escaped,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(nxt, c1),
        });
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);
        assert!(
            verify_module(&m).is_empty(),
            "test setup must start valid before unswitch"
        );

        let pass = LoopUnswitch;
        let changed = pass.run(&mut m);
        assert!(
            !changed,
            "unswitch should bail when loop-defined values escape directly"
        );
        assert!(
            verify_module(&m).is_empty(),
            "bailing out should keep the IR valid"
        );
    }
}
