//! Loop peeling pass.
//!
//! Peels the first iteration of a counted loop when the body contains
//! an equality check against the initial IV value (`if (i == 1)`).
//!
//! The approach: clone the ENTIRE loop as a subgraph using `clone_loop`,
//! then redirect two edges so the clone executes exactly one iteration:
//!   1. Clone's latch back-edge → original loop header (passing init+stride)
//!   2. Clone's cmp exit → original loop's exit block
//!
//! This preserves all internal control flow (if/else branches) in the
//! peeled iteration. Later const-prop folds `iv=init_const` through the
//! clone, turning `if (i==1)` into `if (true)` and eliminating dead code.

use super::loop_utils::{clone_loop, find_preheader, loop_defined_values, resolve_const_int};
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::{find_natural_loops, inst_uses, predecessors, terminator_uses};
use std::collections::HashSet;

pub struct LoopPeel;

impl Pass for LoopPeel {
    fn name(&self) -> &'static str {
        "loop-peel"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if peel_in_function(func) {
                changed = true;
            }
        }
        changed
    }
}

fn peel_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);

    for lp in &loops {
        let Some(ph_id) = find_preheader(func, lp, &preds) else {
            continue;
        };

        // Header must have exactly 1 block param (the IV).
        let hdr = func.block(lp.header);
        if hdr.params.len() != 1 {
            continue;
        }
        let iv = hdr.params[0].id;

        // Get the init value from the preheader's branch to header.
        let init_val = match &func.block(ph_id).terminator {
            Some(Terminator::Branch(dest, args)) if *dest == lp.header && args.len() == 1 => {
                args[0]
            }
            _ => continue,
        };

        // Init must be a compile-time constant.
        let Some(init_const) = resolve_const_int(func, init_val) else {
            continue;
        };

        // Must have a single latch.
        if lp.latches.len() != 1 {
            continue;
        }
        let latch_id = lp.latches[0];

        // Latch must branch back to header with one arg.
        let next_iv = match &func.block(latch_id).terminator {
            Some(Terminator::Branch(dest, args)) if *dest == lp.header && args.len() == 1 => {
                args[0]
            }
            _ => continue,
        };

        // Find stride: next_iv = iadd(iv, stride_const).
        let stride_const = {
            let mut found = None;
            for inst in &func.block(latch_id).insts {
                if inst.id == next_iv {
                    if let InstKind::IAdd(a, b) = &inst.kind {
                        if *a == iv {
                            found = resolve_const_int(func, *b);
                        } else if *b == iv {
                            found = resolve_const_int(func, *a);
                        }
                    }
                    break;
                }
            }
            found
        };
        let Some(stride) = stride_const else { continue };

        // Check if body has a FIRST-ITERATION conditional:
        // ICmp(Eq, iv, init_val) feeding a CondBranch.
        let loop_defs = loop_defined_values(func, lp);
        if has_external_ssa_uses(func, lp, &loop_defs) {
            continue;
        }
        if !has_first_iter_conditional(func, lp, iv, init_const, &loop_defs) {
            continue;
        }

        // Find the loop's exit block (cmp's false-branch target outside the body).
        let exit_block = find_loop_exit(func, lp);
        let Some(exit_id) = exit_block else { continue };

        // Also find the exit args (values passed to exit on the false branch).
        // ---- Perform the peel ----
        do_peel(func, lp, ph_id, init_val, latch_id, exit_id, stride);
        return true; // one at a time
    }
    false
}

/// Only match `ICmp(Eq, iv, val)` where `val` resolves to the same
/// constant as the loop's init value. This is the "first iteration"
/// check. Do NOT match bound checks like `ICmp(Le, iv, 10)`.
fn has_first_iter_conditional(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    iv: ValueId,
    init_const: i64,
    loop_defs: &HashSet<ValueId>,
) -> bool {
    for &bid in &lp.body {
        let block = func.block(bid);
        for inst in &block.insts {
            if let InstKind::ICmp(CmpOp::Eq, a, b) = &inst.kind {
                // One operand must be the IV.
                let (is_iv, other) = if *a == iv {
                    (true, *b)
                } else if *b == iv {
                    (true, *a)
                } else {
                    (false, ValueId(0))
                };
                if !is_iv {
                    continue;
                }

                // The other operand must resolve to the init constant
                // and be loop-invariant.
                if loop_defs.contains(&other) {
                    continue;
                }
                if let Some(c) = resolve_const_int(func, other) {
                    if c == init_const {
                        // Must feed a CondBranch in this block.
                        if let Some(Terminator::CondBranch { cond, .. }) = &block.terminator {
                            if *cond == inst.id {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Find the loop's exit block (first false-branch target outside the body).
fn find_loop_exit(func: &Function, lp: &crate::ir::walk::NaturalLoop) -> Option<BlockId> {
    for &bid in &lp.body {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch { false_dest, .. }) = &block.terminator {
            if !lp.body.contains(false_dest) {
                return Some(*false_dest);
            }
        }
    }
    None
}

/// Peeling adds a new peeled-exit path into the original exit block.
/// If downstream blocks still read loop-defined SSA values directly, the
/// transform would need extra SSA repair across the new predecessor. Until
/// that exists, skip those loops.
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

/// Get the args passed to the exit block from the cmp's false-branch.
fn find_exit_args(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    exit_id: BlockId,
) -> Vec<ValueId> {
    for &bid in &lp.body {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch {
            false_dest,
            false_args,
            ..
        }) = &block.terminator
        {
            if *false_dest == exit_id {
                return false_args.clone();
            }
        }
    }
    Vec::new()
}

/// Perform the peel using clone_loop.
///
/// Strategy:
/// 1. Clone the entire loop as a subgraph (preserves if/else, etc.)
/// 2. Redirect clone's latch → original header (with init+stride)
/// 3. Redirect clone's cmp exit → original exit block
/// 4. Preheader → clone's header (with init)
fn do_peel(
    func: &mut Function,
    lp: &crate::ir::walk::NaturalLoop,
    ph_id: BlockId,
    init_val: ValueId,
    latch_id: BlockId,
    exit_id: BlockId,
    stride: i64,
) {
    // Clone the entire loop.
    let (block_map, _new_blocks) = clone_loop(func, lp);

    // The IV type (needed to emit the init+stride constant).
    let hdr = func.block(lp.header);
    let iv_ty = hdr.params[0].ty.clone();
    let iv_width = match &iv_ty {
        IrType::Int(w) => *w,
        _ => return,
    };
    let init_const = resolve_const_int(func, init_val).unwrap();

    // Emit init+stride constant in a helper block so it's available.
    let next_init_id = func.next_value_id();
    func.register_type(next_init_id, iv_ty.clone());
    // We'll place this constant in the clone's latch before rewriting it.

    // --- Redirect clone's latch ---
    // Clone's latch currently branches to clone's header. Redirect it
    // to the ORIGINAL header, passing init+stride.
    let clone_latch = block_map[&latch_id];
    // Add the init+stride constant to the clone's latch.
    let dummy_span = crate::lexer::Span {
        file_id: 0,
        start: crate::lexer::Position { line: 0, col: 0 },
        end: crate::lexer::Position { line: 0, col: 0 },
    };
    func.block_mut(clone_latch).insts.push(Inst {
        id: next_init_id,
        kind: InstKind::ConstInt((init_const + stride) as i128, iv_width),
        ty: iv_ty.clone(),
        span: dummy_span,
    });
    func.block_mut(clone_latch).terminator =
        Some(Terminator::Branch(lp.header, vec![next_init_id]));

    // --- Redirect clone's cmp exit ---
    // Find the cmp block in the clone (the one with a CondBranch whose
    // false-dest was the clone's exit). Redirect its false-dest to the
    // ORIGINAL exit block with the original exit args.
    for &orig_bid in &lp.body {
        let block = func.block(orig_bid);
        if let Some(Terminator::CondBranch { false_dest, .. }) = &block.terminator {
            if *false_dest == exit_id {
                // This is the cmp block in the original. Find its clone.
                let clone_cmp = block_map[&orig_bid];
                // The clone's false-dest currently points to the cloned exit
                // (which doesn't exist as a separate block since exit_id is
                // outside the loop body and wasn't cloned). Actually, clone_loop
                // only clones blocks IN the body, so the exit_id wasn't cloned.
                // The clone's terminator already points to exit_id — but with
                // remapped args. We need to ensure the exit_args are correct.
                //
                // clone_loop's remap_terminator remaps values but not block
                // targets that are OUTSIDE the body. So the clone's cmp false-dest
                // already points to the original exit_id. The false_args were
                // remapped through val_map. This should be correct as-is.
                //
                // However, we need to double-check: the exit args in the clone
                // might reference cloned values that should reference originals.
                // Actually no — for the peeled iteration, the cloned values ARE
                // the correct ones (they compute the exit condition for iteration 1).
                let _ = clone_cmp; // Already correct — exit_id is outside body.
                break;
            }
        }
    }

    // --- Wire preheader → clone's header ---
    let clone_header = block_map[&lp.header];
    func.block_mut(ph_id).terminator = Some(Terminator::Branch(clone_header, vec![init_val]));
}

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

    #[test]
    fn peel_no_op_on_empty() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopPeel;
        let changed = pass.run(&mut m);
        assert!(!changed, "no loops → no peeling");
    }

    #[test]
    fn peel_no_op_without_eq_check() {
        // Loop with no `if (i == init)` → should not peel.
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let header = f.create_block("header");
        let cmp = f.create_block("cmp");
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
        f.block_mut(header).terminator = Some(Terminator::Branch(cmp, vec![]));

        let cmp_v = f.next_value_id();
        f.register_type(cmp_v, IrType::Bool);
        f.block_mut(cmp).insts.push(Inst {
            id: cmp_v,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c10),
        });
        let exit_seed = f.next_value_id();
        f.register_type(exit_seed, IrType::Int(IntWidth::I32));
        f.block_mut(cmp).insts.push(Inst {
            id: exit_seed,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        f.block_mut(cmp).terminator = Some(Terminator::CondBranch {
            cond: cmp_v,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        // Body has NO equality check — just a store.
        let alloca = f.next_value_id();
        f.register_type(alloca, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(body).insts.push(Inst {
            id: alloca,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let store_id = f.next_value_id();
        f.register_type(store_id, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: store_id,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(iv, alloca),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(latch, vec![]));

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

        let pass = LoopPeel;
        let changed = pass.run(&mut m);
        assert!(!changed, "loop without i==init check should not be peeled");
    }

    #[test]
    fn peel_skips_loops_with_direct_exit_uses_of_loop_values() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let preheader = f.create_block("preheader");
        let header = f.create_block("header");
        let cmp = f.create_block("cmp");
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
        f.block_mut(entry).terminator = Some(Terminator::Branch(preheader, vec![]));
        f.block_mut(preheader).terminator = Some(Terminator::Branch(header, vec![c1]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(header).terminator = Some(Terminator::Branch(cmp, vec![]));

        let cmp_v = f.next_value_id();
        f.register_type(cmp_v, IrType::Bool);
        f.block_mut(cmp).insts.push(Inst {
            id: cmp_v,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c10),
        });
        let exit_seed = f.next_value_id();
        f.register_type(exit_seed, IrType::Int(IntWidth::I32));
        f.block_mut(cmp).insts.push(Inst {
            id: exit_seed,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        f.block_mut(cmp).terminator = Some(Terminator::CondBranch {
            cond: cmp_v,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        let first_iter = f.next_value_id();
        f.register_type(first_iter, IrType::Bool);
        f.block_mut(body).insts.push(Inst {
            id: first_iter,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Eq, iv, c1),
        });
        let alloca = f.next_value_id();
        f.register_type(alloca, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(body).insts.push(Inst {
            id: alloca,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let store_id = f.next_value_id();
        f.register_type(store_id, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: store_id,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(iv, alloca),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(latch, vec![]));

        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![nxt]));

        let escaped = f.next_value_id();
        f.register_type(escaped, IrType::Int(IntWidth::I32));
        f.block_mut(exit).insts.push(Inst {
            id: escaped,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(exit_seed, c1),
        });
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);
        assert!(
            verify_module(&m).is_empty(),
            "test setup must start valid before peeling"
        );

        let pass = LoopPeel;
        let changed = pass.run(&mut m);
        assert!(
            !changed,
            "peeling should skip loops whose exit reads loop-defined SSA values directly"
        );
        assert!(
            verify_module(&m).is_empty(),
            "skipping the peel should keep the IR valid"
        );
    }
}
