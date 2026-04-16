//! Loop interchange pass.
//!
//! Swaps the iteration order of perfectly-nested loop pairs to improve
//! memory access patterns. Critical for Fortran because arrays are
//! column-major: `a(i, j)` is stored with `i` varying fastest. If the
//! inner loop iterates over `j` while `i` is outer, array accesses
//! stride by the column extent on each iteration — cache-hostile.
//! Interchanging makes `i` the inner loop, giving stride-1 access.
//!
//! ## Algorithm
//!
//! 1. Build loop tree, find perfectly-nested pairs.
//! 2. For each pair, detect counted-loop structure (header, IV, bounds).
//! 3. Analyze the body's GEP instructions to determine which IV is used
//!    as the "fast" (first/leftmost) subscript.
//! 4. If the OUTER IV appears as the fast subscript, interchange is
//!    profitable (and almost always legal for simple array assignments).
//! 5. Transform by swapping the branch arguments that carry init values
//!    to each header, and swapping the bounds used in each comparison.
//!
//! ## Legality
//!
//! Conservative: only interchange when all array accesses in the body
//! use both IVs as simple direct subscripts with no cross-iteration
//! read-before-write. This avoids needing full dependence analysis.

use super::loop_tree::build_loop_tree;
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::predecessors;

pub struct LoopInterchange;

impl Pass for LoopInterchange {
    fn name(&self) -> &'static str {
        "loop-interchange"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if interchange_in_function(func) {
                changed = true;
            }
        }
        changed
    }
}

fn interchange_in_function(func: &mut Function) -> bool {
    let tree = build_loop_tree(func);
    let preds = predecessors(func);
    let pairs = tree.perfectly_nested_pairs(func);

    for (outer_id, inner_id) in &pairs {
        let outer = tree.node(*outer_id);
        let inner = tree.node(*inner_id);

        // Both loops must have a recognized counted-loop structure:
        // header(%iv) → cmp_block(icmp, condBr) → body → latch(iadd, br header)
        let Some(outer_shape) = detect_loop_shape(func, outer, &preds) else {
            continue;
        };
        let Some(inner_shape) = detect_loop_shape(func, inner, &preds) else {
            continue;
        };

        // Check profitability: is the outer IV used as the first (fast)
        // subscript of a multi-dimensional array GEP?
        if !should_interchange(func, inner, outer_shape.iv, inner_shape.iv) {
            continue;
        }

        // Check legality: conservative — only if the body has no
        // loop-carried dependencies that would change semantics.
        if !is_interchange_legal(func, inner, outer_shape.iv, inner_shape.iv) {
            continue;
        }

        // Perform the interchange by swapping the loop bounds and inits.
        do_interchange(func, &outer_shape, &inner_shape);
        return true; // one at a time
    }
    false
}

/// Minimal loop shape for interchange.
struct LoopShape {
    header: BlockId,
    cmp_block: BlockId,
    iv: ValueId,    // block param on header
    bound: ValueId, // the upper-bound value in the comparison
    latch: BlockId,
    /// The value passed to the header from the preheader (initial IV).
    init_arg_idx: usize, // index in preheader's branch args
}

fn detect_loop_shape(
    func: &Function,
    node: &super::loop_tree::LoopTreeNode,
    _preds: &std::collections::HashMap<BlockId, Vec<BlockId>>,
) -> Option<LoopShape> {
    let header = node.header;
    let hdr = func.block(header);

    // Header must have exactly 1 block param (the IV).
    if hdr.params.len() != 1 {
        return None;
    }
    let iv = hdr.params[0].id;

    // Header must be a relay (0 instructions, branch to cmp_block).
    if !hdr.insts.is_empty() {
        return None;
    }
    let cmp_block = match &hdr.terminator {
        Some(Terminator::Branch(t, args)) if args.is_empty() => *t,
        _ => return None,
    };
    if !node.body.contains(&cmp_block) {
        return None;
    }

    // Cmp block must have icmp + condBr.
    let cmp_blk = func.block(cmp_block);
    let bound = {
        let mut found_bound = None;
        for inst in &cmp_blk.insts {
            if let InstKind::ICmp(_, a, b) = &inst.kind {
                // One operand should be the IV, the other is the bound.
                if *a == iv {
                    found_bound = Some(*b);
                } else if *b == iv {
                    found_bound = Some(*a);
                }
            }
        }
        found_bound?
    };

    // Find the single latch.
    if node.latches.len() != 1 {
        return None;
    }
    let latch = node.latches[0];

    Some(LoopShape {
        header,
        cmp_block,
        iv,
        bound,
        latch,
        init_arg_idx: 0, // always first param
    })
}

/// Check if interchanging would improve memory access patterns.
///
/// Returns true if the OUTER IV appears as the "fast-varying" (first)
/// subscript in a multi-dimensional array GEP. In column-major Fortran,
/// the first subscript should be the inner loop's IV for stride-1 access.
fn should_interchange(
    func: &Function,
    inner_loop: &super::loop_tree::LoopTreeNode,
    outer_iv: ValueId,
    inner_iv: ValueId,
) -> bool {
    // Scan the inner loop body for GEP instructions that use both IVs.
    for &bid in &inner_loop.body {
        let block = func.block(bid);
        for inst in &block.insts {
            if let InstKind::GetElementPtr(_, indices) = &inst.kind {
                // We're looking for a flat-offset GEP where the offset
                // is computed as: (outer_iv - lo) + (inner_iv - lo) * stride
                // or equivalently: fast_part + slow_part * stride
                //
                // In column-major, the first addend (non-multiplied part)
                // is the "fast" subscript. If the outer IV contributes to
                // the non-multiplied addend, interchange is profitable.
                if let Some(offset_val) = indices.first() {
                    if uses_iv_in_fast_position(func, *offset_val, outer_iv, inner_iv) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Check if the flat offset computation has the outer IV in the
/// non-multiplied (fast) position. The lowered pattern is:
///   %fast = isub %outer_iv, %lo
///   %slow_raw = isub %inner_iv, %lo
///   %slow = imul %slow_raw, %stride
///   %offset = iadd %fast, %slow
///
/// We trace back from the GEP index to find if outer_iv feeds the
/// non-multiplied side of the final iadd.
fn uses_iv_in_fast_position(
    func: &Function,
    offset: ValueId,
    outer_iv: ValueId,
    _inner_iv: ValueId,
) -> bool {
    // Find the instruction that produces `offset`.
    let Some(inst) = find_inst(func, offset) else {
        return false;
    };

    // The offset should be an iadd of two parts.
    let (a, b) = match &inst.kind {
        InstKind::IAdd(a, b) => (*a, *b),
        _ => return false,
    };

    // One side should be the fast part (derived from outer_iv without
    // multiplication), the other should be the slow part (involves imul).
    let a_uses_mul = trace_involves_mul(func, a);
    let b_uses_mul = trace_involves_mul(func, b);

    // The fast part is the one WITHOUT multiplication.
    let fast_part = if !a_uses_mul && b_uses_mul {
        a
    } else if a_uses_mul && !b_uses_mul {
        b
    } else {
        return false;
    };

    // Does the fast part trace back to the outer IV?
    traces_to_iv(func, fast_part, outer_iv)
}

/// Check if a value's computation involves an IMul somewhere.
fn trace_involves_mul(func: &Function, val: ValueId) -> bool {
    let Some(inst) = find_inst(func, val) else {
        return false;
    };
    match &inst.kind {
        InstKind::IMul(..) => true,
        InstKind::IAdd(a, b) | InstKind::ISub(a, b) => {
            trace_involves_mul(func, *a) || trace_involves_mul(func, *b)
        }
        InstKind::IntExtend(a, _, _) => trace_involves_mul(func, *a),
        _ => false,
    }
}

/// Check if a value traces back to a specific IV (through isub, int_extend).
fn traces_to_iv(func: &Function, val: ValueId, iv: ValueId) -> bool {
    if val == iv {
        return true;
    }
    let Some(inst) = find_inst(func, val) else {
        return false;
    };
    match &inst.kind {
        InstKind::ISub(a, _) => traces_to_iv(func, *a, iv),
        InstKind::IntExtend(a, _, _) => traces_to_iv(func, *a, iv),
        _ => false,
    }
}

/// Find the instruction that defines a value.
fn find_inst(func: &Function, vid: ValueId) -> Option<&Inst> {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.id == vid {
                return Some(inst);
            }
        }
    }
    None
}

/// Legality check using dependence analysis. Verifies that swapping
/// the loop order does not reverse any dependence direction.
fn is_interchange_legal(
    func: &Function,
    inner_loop: &super::loop_tree::LoopTreeNode,
    outer_iv: ValueId,
    inner_iv: ValueId,
) -> bool {
    super::dep_analysis::interchange_legal(func, &inner_loop.body, outer_iv, inner_iv)
}

/// Trace through a GEP chain to find the base array pointer.
fn trace_gep_base(func: &Function, ptr: ValueId) -> Option<ValueId> {
    let Some(inst) = find_inst(func, ptr) else {
        return Some(ptr);
    };
    match &inst.kind {
        InstKind::GetElementPtr(base, _) => Some(*base),
        _ => Some(ptr),
    }
}

/// Perform the actual interchange transformation.
///
/// Strategy: swap the init and bound values between the two loops.
/// After swapping, what was the outer loop iterates over the inner
/// range and vice versa.
fn do_interchange(func: &mut Function, outer: &LoopShape, inner: &LoopShape) {
    // Find the preheader of the outer loop (it branches to outer.header
    // with the outer IV init value).
    let outer_preheader = {
        let mut ph = None;
        for block in &func.blocks {
            if let Some(Terminator::Branch(dest, _)) = &block.terminator {
                if *dest == outer.header && block.id != outer.latch {
                    ph = Some(block.id);
                    break;
                }
            }
            if let Some(Terminator::CondBranch {
                true_dest,
                false_dest,
                ..
            }) = &block.terminator
            {
                if *true_dest == outer.header || *false_dest == outer.header {
                    // Could be a condBr preheader from preheader insertion
                    // but we need the unconditional one.
                }
            }
        }
        ph
    };
    let Some(outer_ph) = outer_preheader else {
        return;
    };

    // Find the block that branches to the inner header with the inner
    // IV init value (this is the outer loop's "body entry" block).
    let inner_entry = {
        let mut ie = None;
        for block in &func.blocks {
            if let Some(Terminator::Branch(dest, args)) = &block.terminator {
                if *dest == inner.header && !args.is_empty() && block.id != inner.latch {
                    ie = Some(block.id);
                    break;
                }
            }
            if let Some(Terminator::CondBranch {
                true_dest,
                true_args,
                ..
            }) = &block.terminator
            {
                if *true_dest == inner.header && !true_args.is_empty() {
                    ie = Some(block.id);
                    break;
                }
            }
        }
        ie
    };
    let Some(inner_entry_block) = inner_entry else {
        return;
    };

    // Get current init values.
    let outer_init = get_branch_arg_to(func, outer_ph, outer.header, 0);
    let inner_init = get_branch_arg_to(func, inner_entry_block, inner.header, 0);
    let Some(outer_init_val) = outer_init else {
        return;
    };
    let Some(inner_init_val) = inner_init else {
        return;
    };

    // Swap init values: outer preheader now passes inner's init to
    // outer's header, and inner entry now passes outer's init to
    // inner's header.
    set_branch_arg_to(func, outer_ph, outer.header, 0, inner_init_val);
    set_branch_arg_to(func, inner_entry_block, inner.header, 0, outer_init_val);

    // Swap the bounds in the comparison blocks.
    swap_bound(func, outer.cmp_block, outer.iv, outer.bound, inner.bound);
    swap_bound(func, inner.cmp_block, inner.iv, inner.bound, outer.bound);
}

/// Get the Nth branch argument passed to a target block.
fn get_branch_arg_to(func: &Function, from: BlockId, to: BlockId, idx: usize) -> Option<ValueId> {
    let block = func.block(from);
    match &block.terminator {
        Some(Terminator::Branch(dest, args)) if *dest == to => args.get(idx).copied(),
        Some(Terminator::CondBranch {
            true_dest,
            true_args,
            false_dest,
            false_args,
            ..
        }) => {
            if *true_dest == to {
                true_args.get(idx).copied()
            } else if *false_dest == to {
                false_args.get(idx).copied()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Set the Nth branch argument passed to a target block.
fn set_branch_arg_to(func: &mut Function, from: BlockId, to: BlockId, idx: usize, val: ValueId) {
    let block = func.block_mut(from);
    match &mut block.terminator {
        Some(Terminator::Branch(dest, args)) if *dest == to => {
            if idx < args.len() {
                args[idx] = val;
            }
        }
        Some(Terminator::CondBranch {
            true_dest,
            true_args,
            false_dest,
            false_args,
            ..
        }) => {
            if *true_dest == to && idx < true_args.len() {
                true_args[idx] = val;
            } else if *false_dest == to && idx < false_args.len() {
                false_args[idx] = val;
            }
        }
        _ => {}
    }
}

/// Swap the bound value in a comparison block's ICmp instruction.
fn swap_bound(
    func: &mut Function,
    cmp_block: BlockId,
    iv: ValueId,
    old_bound: ValueId,
    new_bound: ValueId,
) {
    let block = func.block_mut(cmp_block);
    for inst in &mut block.insts {
        if let InstKind::ICmp(_op, a, b) = &mut inst.kind {
            if *a == iv && *b == old_bound {
                *b = new_bound;
                return;
            } else if *b == iv && *a == old_bound {
                *a = new_bound;
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
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
    fn interchange_pass_builds() {
        // Smoke test: the pass can be constructed and run on an empty module.
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopInterchange;
        let changed = pass.run(&mut m);
        assert!(!changed, "no loops → no interchange");
    }
}
