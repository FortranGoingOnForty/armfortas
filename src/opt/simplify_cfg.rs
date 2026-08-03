//! SimplifyCFG pass: merge empty block chains and thread jumps.
//!
//! After inlining, the CFG often contains blocks that simply branch
//! unconditionally to the next block with no instructions or block
//! params. This pass merges them:
//!
//! ```text
//! Before: A: br B   B: br C   C: [real code]
//! After:  A: br C             C: [real code]
//! ```
//!
//! Also merges a predecessor into its successor when:
//!   - The predecessor's only terminator is `Branch(succ, args)`
//!   - The successor has exactly one predecessor
//!   - Neither block is the function entry
//!
//! This eliminates the empty block chains left by inlining.

use super::loop_utils::find_preheader;
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};
use std::collections::HashSet;

pub struct SimplifyCfg;

impl Pass for SimplifyCfg {
    fn name(&self) -> &'static str {
        "simplify-cfg"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if simplify_function(func) {
                changed = true;
            }
        }
        changed
    }
}

fn simplify_function(func: &mut Function) -> bool {
    let mut changed = false;

    // Preheader insertion deliberately creates dedicated edge blocks before
    // loop headers. A zero-parameter preheader is otherwise indistinguishable
    // from an empty forwarding block, so deleting it here makes the two passes
    // remove and recreate the same block forever.
    let loop_preheaders: HashSet<BlockId> = {
        let preds = predecessors(func);
        find_natural_loops(func)
            .iter()
            .filter_map(|lp| find_preheader(func, lp, &preds))
            .collect()
    };

    // Phase 1: Thread jumps through empty blocks.
    // If block B has no instructions, no block params, and terminates
    // with Branch(C, []), replace all branches to B with branches to C.
    loop {
        let mut redirect: Option<(BlockId, BlockId)> = None;

        for block in &func.blocks {
            if block.id == func.entry {
                continue;
            }
            if loop_preheaders.contains(&block.id) {
                continue;
            }
            if !block.insts.is_empty() {
                continue;
            }
            if !block.params.is_empty() {
                continue;
            }
            if let Some(Terminator::Branch(target, args)) = &block.terminator {
                if args.is_empty() && *target != block.id {
                    // This block is empty and just forwards to target.
                    // A self-loop has no distinct forwarding destination:
                    // replacing its terminator would change nontermination
                    // into `Unreachable`.
                    redirect = Some((block.id, *target));
                    break;
                }
            }
        }

        let Some((empty_block, target)) = redirect else {
            break;
        };

        // Redirect all predecessors of empty_block to target instead.
        for block in &mut func.blocks {
            if block.id == empty_block {
                continue;
            } // don't redirect self
            let term = match &mut block.terminator {
                Some(t) => t,
                None => continue,
            };
            redirect_terminator(term, empty_block, target);
        }
        // Mark the empty block as unreachable so we don't find it again.
        func.block_mut(empty_block).terminator = Some(Terminator::Unreachable);
        changed = true;
    }

    // Phase 2: Merge blocks where predecessor has unconditional branch
    // to a successor with exactly one predecessor.
    loop {
        let preds = predecessors(func);
        let mut merge: Option<(BlockId, BlockId)> = None;

        for block in &func.blocks {
            if let Some(Terminator::Branch(succ, args)) = &block.terminator {
                if args.is_empty() && *succ != block.id {
                    // Check if successor has exactly one predecessor.
                    let succ_preds = preds.get(succ);
                    if let Some(sp) = succ_preds {
                        let mut unique: Vec<BlockId> = sp.clone();
                        unique.sort_by_key(|b| b.0);
                        unique.dedup();
                        if unique.len() == 1 && unique[0] == block.id {
                            // Can merge: append successor's contents to predecessor.
                            merge = Some((block.id, *succ));
                            break;
                        }
                    }
                }
            }
        }

        let Some((pred_id, succ_id)) = merge else {
            break;
        };

        // Merge: append successor's instructions and terminator to predecessor.
        let succ_insts = func.block(succ_id).insts.clone();
        let succ_term = func.block(succ_id).terminator.clone();
        func.block_mut(pred_id).insts.extend(succ_insts);
        func.block_mut(pred_id).terminator = succ_term;

        // Mark successor as unreachable (will be pruned).
        func.block_mut(succ_id).terminator = Some(Terminator::Unreachable);
        func.block_mut(succ_id).insts.clear();

        changed = true;
    }

    // Prune unreachable blocks.
    if changed {
        crate::ir::walk::prune_unreachable(func);
    }

    changed
}

/// Redirect all edges targeting `old` to `new` in a terminator.
fn redirect_terminator(term: &mut Terminator, old: BlockId, new: BlockId) {
    match term {
        Terminator::Branch(dest, _) if *dest == old => {
            *dest = new;
        }
        Terminator::CondBranch {
            true_dest,
            false_dest,
            ..
        } => {
            if *true_dest == old {
                *true_dest = new;
            }
            if *false_dest == old {
                *false_dest = new;
            }
        }
        Terminator::Switch { default, cases, .. } => {
            if *default == old {
                *default = new;
            }
            for (_, dest) in cases.iter_mut() {
                if *dest == old {
                    *dest = new;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::opt::pass::{Pass, PassManager};
    use crate::opt::preheader::PreheaderInsert;

    fn bool_const(func: &mut Function, value: bool) -> ValueId {
        let id = func.next_value_id();
        func.register_type(id, IrType::Bool);
        func.block_mut(func.entry).insts.push(Inst {
            id,
            ty: IrType::Bool,
            span: crate::lexer::Span {
                file_id: 0,
                start: crate::lexer::Position { line: 0, col: 0 },
                end: crate::lexer::Position { line: 0, col: 0 },
            },
            kind: InstKind::ConstBool(value),
        });
        id
    }

    #[test]
    fn preserves_empty_preheader_across_fixpoint_iterations() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        let preheader = func.create_block("preheader");
        let header = func.create_block("header");
        let body = func.create_block("body");
        let exit = func.create_block("exit");
        let cond = bool_const(&mut func, true);

        func.block_mut(func.entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: preheader,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });
        func.block_mut(preheader).terminator = Some(Terminator::Branch(header, vec![]));
        func.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });
        func.block_mut(body).terminator = Some(Terminator::Branch(header, vec![]));
        func.block_mut(exit).terminator = Some(Terminator::Return(None));
        module.add_function(func);

        let mut manager = PassManager::new();
        manager.max_iterations = 4;
        manager.add(Box::new(SimplifyCfg));
        manager.add(Box::new(PreheaderInsert));
        manager
            .run(&mut module)
            .expect("CFG simplification and preheader insertion must converge");

        let func = &module.functions[0];
        let preds = predecessors(func);
        let loops = find_natural_loops(func);
        assert_eq!(loops.len(), 1);
        assert_eq!(find_preheader(func, &loops[0], &preds), Some(preheader));
    }

    #[test]
    fn merges_empty_chain() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let b = f.create_block("middle");
        let c = f.create_block("end");
        // entry → b → c → ret
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(b, vec![]));
        f.block_mut(b).terminator = Some(Terminator::Branch(c, vec![]));
        f.block_mut(c).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = SimplifyCfg;
        let changed = pass.run(&mut m);
        assert!(changed);

        // After: entry should branch directly to end (or entry should
        // contain the ret directly).
        let f = &m.functions[0];
        // The chain should be collapsed.
        assert!(
            f.blocks.len() <= 2,
            "should merge empty chain, got {} blocks",
            f.blocks.len()
        );
    }

    #[test]
    fn preserves_empty_infinite_self_loop() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut function = Function::new("test".into(), vec![], IrType::Void);
        let loop_block = function.create_block("forever");
        function.block_mut(function.entry).terminator =
            Some(Terminator::Branch(loop_block, vec![]));
        function.block_mut(loop_block).terminator = Some(Terminator::Branch(loop_block, vec![]));
        module.add_function(function);

        assert!(
            !SimplifyCfg.run(&mut module),
            "an empty self-loop has no forwarding edge to simplify"
        );
        assert!(
            matches!(
                module.functions[0].block(loop_block).terminator.as_ref(),
                Some(Terminator::Branch(target, args))
                    if *target == loop_block && args.is_empty()
            ),
            "CFG simplification converted nontermination into termination"
        );
    }

    #[test]
    fn collapses_empty_cycle_without_destroying_nontermination() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut function = Function::new("test".into(), vec![], IrType::Void);
        let left = function.create_block("left");
        let right = function.create_block("right");
        function.block_mut(function.entry).terminator = Some(Terminator::Branch(left, vec![]));
        function.block_mut(left).terminator = Some(Terminator::Branch(right, vec![]));
        function.block_mut(right).terminator = Some(Terminator::Branch(left, vec![]));
        module.add_function(function);

        assert!(
            SimplifyCfg.run(&mut module),
            "a two-block empty cycle can be collapsed to one block"
        );
        let function = &module.functions[0];
        let Some(Terminator::Branch(loop_block, args)) =
            function.block(function.entry).terminator.as_ref()
        else {
            panic!("entry must still branch into the infinite cycle");
        };
        assert!(args.is_empty());
        assert!(
            matches!(
                function.block(*loop_block).terminator.as_ref(),
                Some(Terminator::Branch(target, args))
                    if target == loop_block && args.is_empty()
            ),
            "cycle collapse must leave a reachable self-loop"
        );
    }

    #[test]
    fn no_op_on_non_empty_blocks() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let b = f.create_block("body");
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(b, vec![]));
        // Body has an instruction — should not be merged.
        let id = f.next_value_id();
        f.register_type(id, IrType::Int(IntWidth::I32));
        f.block_mut(b).insts.push(Inst {
            id,
            ty: IrType::Int(IntWidth::I32),
            span: crate::lexer::Span {
                file_id: 0,
                start: crate::lexer::Position { line: 0, col: 0 },
                end: crate::lexer::Position { line: 0, col: 0 },
            },
            kind: InstKind::ConstInt(42, IntWidth::I32),
        });
        f.block_mut(b).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = SimplifyCfg;
        let _changed = pass.run(&mut m);
        // Body block may be merged into entry (single pred + unconditional branch).
        // But the instruction should be preserved.
        let f = &m.functions[0];
        let has_const = f.blocks.iter().any(|bl| {
            bl.insts
                .iter()
                .any(|i| matches!(i.kind, InstKind::ConstInt(42, _)))
        });
        assert!(has_const, "const instruction must be preserved");
    }
}
