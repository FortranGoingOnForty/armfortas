//! Jump threading pass.
//!
//! Pattern handled (the most common one Fortran lowering produces
//! after inlining + SCCP):
//!
//! ```text
//! pred1 → join(p=const_true)  ┐
//! pred2 → join(p=const_false) │   "branch through join"
//!         ...                 │
//! join:   br p, A, B          ┘
//! ```
//!
//! When `join` has no instructions (or only the block param) and ends
//! in a `CondBranch` whose `cond` is exactly the block parameter `p`,
//! and every reachable predecessor passes a *constant* value for `p`,
//! we can rewrite each predecessor's branch to jump directly to the
//! corresponding successor (`A` if `p=true`, `B` if `p=false`).
//!
//! This is intentionally narrower than full Lengauer-Tarjan-based
//! threading. The conservative version above:
//!  * handles the post-SCCP "every pred passes a literal" case;
//!  * never duplicates instructions (no code growth);
//!  * never creates irreducible CFGs (each pred just rewires its
//!    existing terminator);
//!  * leaves any predecessor that *doesn't* pass a constant alone,
//!    so partial threading is fine.
//!
//! After threading, `join` may become unreachable (every pred
//! re-routed). `prune_unreachable` mops up.
//!
//! Note: simplify_cfg's existing "merge B into pred" pass handles a
//! related but disjoint pattern (B has zero params and is reached via
//! a single empty predecessor). Threading is needed when the join
//! has params or multiple preds.

use super::pass::Pass;
use super::util::prune_unreachable;
use crate::ir::inst::*;
use std::collections::{HashMap, HashSet};

pub struct JumpThread;

impl Pass for JumpThread {
    fn name(&self) -> &'static str {
        "jump-thread"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if thread_func(func) {
                changed = true;
            }
        }
        changed
    }
}

fn thread_func(func: &mut Function) -> bool {
    // Build constant map for inst-defined values.
    let mut consts: HashMap<ValueId, ConstKey> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(ck) = ConstKey::from_inst(&inst.kind) {
                consts.insert(inst.id, ck);
            }
        }
    }

    // For each candidate join block, plan the rewrite.
    // A candidate join:
    //   * has at least one BlockParam
    //   * has zero non-constant instructions (we allow ConstX
    //     instructions to remain — they're trivially copyable
    //     downstream)
    //   * ends in CondBranch where `cond` is one of its own params
    //
    // Plan: for each predecessor whose branch arg for that param is
    // a known constant, rewire pred's terminator to the matching
    // successor. Each rewire copies the join's other branch args
    // through unchanged.
    //
    // We iterate to a fixpoint at the function level — each thread
    // can expose new opportunities.
    let mut any_changed = false;
    let mut iter_changed = true;
    while iter_changed {
        iter_changed = false;
        let plan = plan_threads(func, &consts);
        if plan.is_empty() {
            break;
        }
        for action in plan {
            apply_thread(func, &action);
            iter_changed = true;
            any_changed = true;
        }
        // Refresh consts since we may have inserted nothing yet, but
        // do it for safety in case a future extension materializes
        // new constants.
        consts.clear();
        for block in &func.blocks {
            for inst in &block.insts {
                if let Some(ck) = ConstKey::from_inst(&inst.kind) {
                    consts.insert(inst.id, ck);
                }
            }
        }
    }

    if any_changed {
        let _ = prune_unreachable(func);
        func.rebuild_type_cache();
    }
    any_changed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ConstKey {
    Bool(bool),
    Int(i128),
}

impl ConstKey {
    fn from_inst(k: &InstKind) -> Option<Self> {
        match k {
            InstKind::ConstBool(b) => Some(ConstKey::Bool(*b)),
            InstKind::ConstInt(v, _) => Some(ConstKey::Int(*v)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct ThreadAction {
    pred: BlockId,
    /// The join block whose terminator this pred should bypass.
    join: BlockId,
    /// New successor (chosen from join's CondBranch arms).
    new_dest: BlockId,
    /// Args to pass on the new edge (the CondBranch's matching
    /// arm args, with each `cond` param substitution pre-resolved
    /// is unnecessary — we pass them verbatim).
    new_args: Vec<ValueId>,
    /// Reason: which arm did we pick (true=true_dest, false=false_dest).
    /// Used only for debug tracing.
    #[allow(dead_code)]
    arm: bool,
}

fn plan_threads(func: &Function, consts: &HashMap<ValueId, ConstKey>) -> Vec<ThreadAction> {
    // Block id → (cond_param_id, true_dest, true_args, false_dest, false_args)
    let mut actions: Vec<ThreadAction> = Vec::new();
    let mut seen_pred_join: HashSet<(BlockId, BlockId)> = HashSet::new();

    for join in &func.blocks {
        // Skip blocks with non-const instructions.
        let has_only_consts = join
            .insts
            .iter()
            .all(|i| ConstKey::from_inst(&i.kind).is_some());
        if !has_only_consts {
            continue;
        }
        if join.params.is_empty() {
            continue;
        }
        let Some(Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        }) = join.terminator.clone()
        else {
            continue;
        };
        // Find which param `cond` corresponds to.
        let Some(cond_idx) = join.params.iter().position(|p| p.id == cond) else {
            continue;
        };
        // For each predecessor, see what value flows in for that param.
        for pred_block in &func.blocks {
            let Some(term) = &pred_block.terminator else {
                continue;
            };
            // Find an edge to this join and extract the args.
            let edge_args = match term {
                Terminator::Branch(d, a) if *d == join.id => Some(a.clone()),
                Terminator::CondBranch {
                    true_dest: td,
                    true_args: ta,
                    false_dest: fd,
                    false_args: fa,
                    ..
                } => {
                    // It's possible — though weird — for a CondBranch
                    // to target the same join from both arms. We
                    // process arm-by-arm. For now skip if both target
                    // the join (rewriting would need to split the
                    // arms separately, low value).
                    if *td == join.id && *fd == join.id {
                        None
                    } else if *td == join.id {
                        Some(ta.clone())
                    } else if *fd == join.id {
                        Some(fa.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let Some(args) = edge_args else { continue };
            let Some(passed) = args.get(cond_idx).copied() else {
                continue;
            };
            let Some(ck) = consts.get(&passed).copied() else {
                continue;
            };
            let chosen_arm = match ck {
                ConstKey::Bool(b) => b,
                ConstKey::Int(_) => continue, // not a bool cond
            };
            // Build args for the new edge: the join's matching arm
            // args. If those args reference the cond param itself,
            // substitute the constant value passed by this pred (but
            // since the param is being eliminated by skipping the
            // join entirely, we can rewrite any direct reference to
            // it into the constant — only if the *target* successor
            // accepts it). For simplicity, only thread when the arm
            // args don't include the cond param itself; otherwise
            // we'd need to rewrite via a fresh per-pred constant.
            let arm_args = if chosen_arm { &true_args } else { &false_args };
            if arm_args.contains(&cond) {
                continue;
            }
            // Also: arm args may reference the join's *other* block
            // params. Each of those is passed by this pred via the
            // pred's own arg list at that index. We can rewrite
            // `arm_arg = some_join_param` to the matching pred arg.
            let mut new_args = Vec::with_capacity(arm_args.len());
            let mut bail = false;
            for a in arm_args {
                if let Some(idx) = join.params.iter().position(|p| p.id == *a) {
                    if let Some(v) = args.get(idx).copied() {
                        new_args.push(v);
                    } else {
                        bail = true;
                        break;
                    }
                } else {
                    new_args.push(*a);
                }
            }
            if bail {
                continue;
            }
            // Don't queue the same (pred, join) twice — one rewrite
            // per terminator slot.
            if !seen_pred_join.insert((pred_block.id, join.id)) {
                continue;
            }
            actions.push(ThreadAction {
                pred: pred_block.id,
                join: join.id,
                new_dest: if chosen_arm { true_dest } else { false_dest },
                new_args,
                arm: chosen_arm,
            });
        }
    }
    actions
}

fn apply_thread(func: &mut Function, action: &ThreadAction) {
    let Some(pred_block) = func.blocks.iter_mut().find(|b| b.id == action.pred) else {
        return;
    };
    let Some(term) = pred_block.terminator.as_mut() else {
        return;
    };
    match term {
        Terminator::Branch(d, args) if *d == action.join => {
            *d = action.new_dest;
            *args = action.new_args.clone();
        }
        Terminator::CondBranch {
            true_dest,
            true_args,
            false_dest,
            false_args,
            ..
        } => {
            if *true_dest == action.join {
                *true_dest = action.new_dest;
                *true_args = action.new_args.clone();
            } else if *false_dest == action.join {
                *false_dest = action.new_dest;
                *false_args = action.new_args.clone();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span {
            start: p,
            end: p,
            file_id: 0,
        }
    }

    /// pred1 → join(true)
    /// pred2 → join(false)
    /// join:  cond_branch p, A, B
    /// Expect: pred1 → A, pred2 → B, join becomes unreachable.
    #[test]
    fn threads_two_const_preds_through_join() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let bb_pre1 = f.create_block("pre1");
        let bb_pre2 = f.create_block("pre2");
        let bb_join = f.create_block("join");
        let bb_a = f.create_block("a");
        let bb_b = f.create_block("b");

        let join_p = f.next_value_id();
        f.block_mut(bb_join).params.push(BlockParam {
            id: join_p,
            ty: IrType::Bool,
        });

        let const_t = f.next_value_id();
        let const_f = f.next_value_id();

        // Entry → pre1 (just to make pre1 reachable from entry).
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(bb_pre1, vec![]));
        // Make pre2 reachable too. Add a trivial first cond.
        let entry_cond = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: entry_cond,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: entry_cond,
            true_dest: bb_pre1,
            true_args: vec![],
            false_dest: bb_pre2,
            false_args: vec![],
        });

        f.block_mut(bb_pre1).insts.push(Inst {
            id: const_t,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(bb_pre1).terminator = Some(Terminator::Branch(bb_join, vec![const_t]));

        f.block_mut(bb_pre2).insts.push(Inst {
            id: const_f,
            kind: InstKind::ConstBool(false),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(bb_pre2).terminator = Some(Terminator::Branch(bb_join, vec![const_f]));

        f.block_mut(bb_join).terminator = Some(Terminator::CondBranch {
            cond: join_p,
            true_dest: bb_a,
            true_args: vec![],
            false_dest: bb_b,
            false_args: vec![],
        });
        f.block_mut(bb_a).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_b).terminator = Some(Terminator::Return(None));
        f.rebuild_type_cache();
        m.add_function(f);

        assert!(JumpThread.run(&mut m), "should thread");

        let f = &m.functions[0];
        let pre1 = f.blocks.iter().find(|b| b.id == bb_pre1).unwrap();
        match &pre1.terminator {
            Some(Terminator::Branch(d, args)) => {
                assert_eq!(*d, bb_a);
                assert!(args.is_empty());
            }
            other => panic!("pre1 should branch directly to A, got {:?}", other),
        }
        let pre2 = f.blocks.iter().find(|b| b.id == bb_pre2).unwrap();
        match &pre2.terminator {
            Some(Terminator::Branch(d, _)) => assert_eq!(*d, bb_b),
            other => panic!("pre2 should branch directly to B, got {:?}", other),
        }
        // Join should be pruned since both preds were rerouted.
        assert!(
            !f.blocks.iter().any(|b| b.id == bb_join),
            "join should be unreachable and pruned"
        );
    }

    /// Pred passes a non-constant value for the cond param — must
    /// not thread.
    #[test]
    fn does_not_thread_non_const_pred() {
        let mut m = Module::new("t".into());
        let params = vec![Param {
            name: "x".into(),
            ty: IrType::Bool,
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut f = Function::new("f".into(), params, IrType::Void);
        let bb_join = f.create_block("join");
        let bb_a = f.create_block("a");
        let bb_b = f.create_block("b");
        let join_p = f.next_value_id();
        f.block_mut(bb_join).params.push(BlockParam {
            id: join_p,
            ty: IrType::Bool,
        });
        // Entry passes function param `x` (non-constant) into join.
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(bb_join, vec![ValueId(0)]));
        f.block_mut(bb_join).terminator = Some(Terminator::CondBranch {
            cond: join_p,
            true_dest: bb_a,
            true_args: vec![],
            false_dest: bb_b,
            false_args: vec![],
        });
        f.block_mut(bb_a).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_b).terminator = Some(Terminator::Return(None));
        f.rebuild_type_cache();
        m.add_function(f);

        assert!(!JumpThread.run(&mut m));
        // Entry still branches to the join.
        let entry_term = m.functions[0]
            .blocks
            .iter()
            .find(|b| b.id == m.functions[0].entry)
            .unwrap()
            .terminator
            .clone();
        match entry_term {
            Some(Terminator::Branch(d, _)) => assert_eq!(d, bb_join),
            other => panic!("entry should still branch to join, got {:?}", other),
        }
    }

    /// Block with a real instruction (e.g. Load) is not a candidate
    /// — threading would skip the side-effecting work.
    #[test]
    fn does_not_thread_when_join_has_inst() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let bb_join = f.create_block("join");
        let bb_a = f.create_block("a");
        let bb_b = f.create_block("b");
        let join_p = f.next_value_id();
        f.block_mut(bb_join).params.push(BlockParam {
            id: join_p,
            ty: IrType::Bool,
        });
        // Add a non-const inst (alloca) — must block threading.
        let allo = f.next_value_id();
        f.block_mut(bb_join).insts.push(Inst {
            id: allo,
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: dummy_span(),
        });
        let const_t = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: const_t,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(bb_join, vec![const_t]));
        f.block_mut(bb_join).terminator = Some(Terminator::CondBranch {
            cond: join_p,
            true_dest: bb_a,
            true_args: vec![],
            false_dest: bb_b,
            false_args: vec![],
        });
        f.block_mut(bb_a).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_b).terminator = Some(Terminator::Return(None));
        f.rebuild_type_cache();
        m.add_function(f);

        assert!(!JumpThread.run(&mut m));
    }
}
