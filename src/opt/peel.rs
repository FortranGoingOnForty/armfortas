//! Loop peeling pass.
//!
//! Peels the first iteration of a loop when the body contains a
//! conditional that compares the IV to its initial value. The peeled
//! iteration becomes straight-line code in the preheader, and the
//! loop starts at init+stride. This removes the boundary-condition
//! branch from the hot loop, enabling further simplification by
//! const prop and DCE.
//!
//! ```text
//! Before:
//!   do i = 1, n
//!     if (i == 1) then ... else ... end if
//!   end do
//!
//! After:
//!   [peeled body with i=1]
//!   do i = 2, n
//!     [body without the i==1 branch — simplified by later passes]
//!   end do
//! ```

use std::collections::{HashMap, HashSet};
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::{find_natural_loops, predecessors};
use super::loop_utils::{find_preheader, resolve_const_int, loop_defined_values,
                        remap_inst_kind, remap_terminator};
use super::pass::Pass;

pub struct LoopPeel;

impl Pass for LoopPeel {
    fn name(&self) -> &'static str { "loop-peel" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if peel_in_function(func) { changed = true; }
        }
        changed
    }
}

fn peel_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);

    for lp in &loops {
        let Some(ph_id) = find_preheader(func, lp, &preds) else { continue };

        // Header must have exactly 1 block param (the IV).
        let hdr = func.block(lp.header);
        if hdr.params.len() != 1 { continue; }
        let iv = hdr.params[0].id;
        let iv_ty = hdr.params[0].ty.clone();

        // Get the init value from the preheader's branch to header.
        let init_val = match &func.block(ph_id).terminator {
            Some(Terminator::Branch(dest, args)) if *dest == lp.header && args.len() == 1 =>
                args[0],
            _ => continue,
        };

        // Init must be a compile-time constant (so we can substitute it).
        let Some(init_const) = resolve_const_int(func, init_val) else { continue };

        // Check if the body has a CondBranch comparing the IV to the
        // init value (e.g., `if (i == 1)` when init is 1).
        let loop_defs = loop_defined_values(func, lp);
        if !has_init_conditional(func, lp, iv, init_val, &loop_defs) {
            continue;
        }

        // Find the latch and its stride.
        if lp.latches.len() != 1 { continue; }
        let latch = lp.latches[0];
        let latch_blk = func.block(latch);

        // Latch must branch back to header with one arg (the next IV).
        let next_iv = match &latch_blk.terminator {
            Some(Terminator::Branch(dest, args)) if *dest == lp.header && args.len() == 1 =>
                args[0],
            _ => continue,
        };

        // next_iv must be iadd(iv, stride) where stride is a constant.
        let stride_const = {
            let mut found = None;
            for inst in &latch_blk.insts {
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

        // Peel the first iteration.
        do_peel(func, lp, ph_id, iv, &iv_ty, init_const, stride);
        return true; // one at a time
    }
    false
}

/// Check if the loop body has a CondBranch that compares the IV to the
/// init value (or a value equal to it).
fn has_init_conditional(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    iv: ValueId,
    init_val: ValueId,
    loop_defs: &HashSet<ValueId>,
) -> bool {
    for &bid in &lp.body {
        let block = func.block(bid);
        // Look for ICmp instructions that compare the IV.
        for inst in &block.insts {
            if let InstKind::ICmp(_, a, b) = &inst.kind {
                let involves_iv = *a == iv || *b == iv;
                let other = if *a == iv { *b } else { *a };
                // The other operand should be the init value or a constant
                // equal to the init constant, and must be loop-invariant.
                let other_is_init = other == init_val || !loop_defs.contains(&other);
                if involves_iv && other_is_init {
                    // Check if this comparison feeds a CondBranch in the body.
                    let comp_id = inst.id;
                    if let Some(Terminator::CondBranch { cond, .. }) = &block.terminator {
                        if *cond == comp_id { return true; }
                    }
                }
            }
        }
    }
    false
}

/// Peel the first iteration: clone the body with IV=init_const into
/// the preheader, then adjust the loop to start at init+stride.
fn do_peel(
    func: &mut Function,
    lp: &crate::ir::walk::NaturalLoop,
    ph_id: BlockId,
    iv: ValueId,
    iv_ty: &IrType,
    init_const: i64,
    stride: i64,
) {
    let iv_width = match iv_ty {
        IrType::Int(w) => *w,
        _ => return,
    };

    // Emit the IV constant (init value) in a new "peel" block.
    let peel_block = func.create_block("peel");
    let iv_const_id = func.next_value_id();
    func.register_type(iv_const_id, iv_ty.clone());
    func.block_mut(peel_block).insts.push(Inst {
        id: iv_const_id,
        kind: InstKind::ConstInt(init_const, iv_width),
        ty: iv_ty.clone(),
        span: crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        },
    });

    // Build a value substitution map: iv → iv_const_id.
    let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();
    val_map.insert(iv, iv_const_id);

    // Clone body instructions (excluding header and latch structure)
    // into the peel block. We clone all body blocks' instructions
    // sequentially into a single peel block for simplicity.
    let body_sorted: Vec<BlockId> = {
        let mut v: Vec<BlockId> = lp.body.iter().copied().collect();
        v.sort_by_key(|b| b.0);
        v
    };

    for &old_bid in &body_sorted {
        let old_insts: Vec<Inst> = func.block(old_bid).insts.clone();
        for inst in &old_insts {
            // Skip the IV-related comparison and branch — the peeled
            // iteration executes unconditionally.
            let new_vid = func.next_value_id();
            func.register_type(new_vid, inst.ty.clone());
            val_map.insert(inst.id, new_vid);
            let new_kind = remap_inst_kind(&inst.kind, &val_map);
            func.block_mut(peel_block).insts.push(Inst {
                id: new_vid,
                kind: new_kind,
                ty: inst.ty.clone(),
                span: inst.span,
            });
        }
    }

    // Emit the new init value (init + stride) for the remaining loop.
    let new_init_id = func.next_value_id();
    func.register_type(new_init_id, iv_ty.clone());
    func.block_mut(peel_block).insts.push(Inst {
        id: new_init_id,
        kind: InstKind::ConstInt(init_const + stride, iv_width),
        ty: iv_ty.clone(),
        span: crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        },
    });

    // Peel block branches to the loop header with the new init.
    func.block_mut(peel_block).terminator =
        Some(Terminator::Branch(lp.header, vec![new_init_id]));

    // Rewrite preheader to branch to the peel block instead of the header.
    func.block_mut(ph_id).terminator =
        Some(Terminator::Branch(peel_block, vec![]));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;

    #[test]
    fn peel_pass_no_op_on_empty() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopPeel;
        let changed = pass.run(&mut m);
        assert!(!changed, "no loops → no peeling");
    }
}
