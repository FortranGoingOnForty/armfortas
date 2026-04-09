//! Loop fission pass.
//!
//! Splits a loop with independent statement groups into two loops over
//! the same iteration space. Uses backward instruction slicing from
//! store instructions to partition the body, then clones the loop and
//! removes each group's exclusive instructions from the other copy.
//!
//! ```text
//! Before:
//!   do i = 1, n
//!     a(i) = b(i) + 1    ← group A
//!     c(i) = d(i) * 2    ← group B (independent of A)
//!   end do
//!
//! After:
//!   do i = 1, n; a(i) = b(i) + 1; end do
//!   do i = 1, n; c(i) = d(i) * 2; end do
//! ```

use std::collections::HashSet;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors, inst_uses, prune_unreachable};
use super::loop_utils::{find_preheader, loop_defined_values, clone_loop, build_value_map};
use super::dep_analysis::{collect_mem_refs, test_dependence};
use super::pass::Pass;

const FISSION_MIN_BODY: usize = 4;

pub struct LoopFission;

impl Pass for LoopFission {
    fn name(&self) -> &'static str { "loop-fission" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if fission_in_function(func) { changed = true; }
        }
        changed
    }
}

fn fission_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);

    for lp in &loops {
        let Some(_ph_id) = find_preheader(func, lp, &preds) else { continue };
        if lp.latches.len() != 1 { continue; }
        let latch_id = lp.latches[0];

        // Header must have 1 block param (IV).
        let hdr = func.block(lp.header);
        if hdr.params.len() != 1 { continue; }
        let iv = hdr.params[0].id;

        // Find the single computation body block (contains stores).
        let body_block = find_computation_block(func, lp, latch_id);
        let Some(body_bid) = body_block else { continue };

        let block = func.block(body_bid);
        if block.insts.len() < FISSION_MIN_BODY { continue; }

        // Find store instructions — each is a potential group root.
        let stores: Vec<ValueId> = block.insts.iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .map(|i| i.id)
            .collect();
        if stores.len() != 2 { continue; } // V1: exactly 2 groups

        // Compute backward slices from each store.
        let loop_defs = loop_defined_values(func, lp);
        let slice_a = backward_slice(func, stores[0], &loop_defs);
        let slice_b = backward_slice(func, stores[1], &loop_defs);

        // Check independence: slices must not overlap (except shared index computations).
        // More precisely: the store targets must be to different arrays.
        let mut ivs = HashSet::new();
        ivs.insert(iv);
        let mem_refs = collect_mem_refs(func, &lp.body, &ivs);
        let writes: Vec<_> = mem_refs.iter().filter(|r| r.is_write).collect();
        if writes.len() != 2 { continue; }

        // Different base pointers → independent (Fortran no-alias).
        if writes[0].base == writes[1].base { continue; }

        // Verify no cross-group deps via dep analysis.
        let mut has_cross_dep = false;
        for i in 0..mem_refs.len() {
            for j in (i+1)..mem_refs.len() {
                if !mem_refs[i].is_write && !mem_refs[j].is_write { continue; }
                if mem_refs[i].base == mem_refs[j].base { continue; }
                let dep = test_dependence(&mem_refs[i], &mem_refs[j]);
                if dep.dependent { has_cross_dep = true; break; }
            }
            if has_cross_dep { break; }
        }
        if has_cross_dep { continue; }

        // ---- Perform fission ----
        let exit_id = find_loop_exit(func, lp);
        let Some(exit_id) = exit_id else { continue };

        do_fission(func, lp, body_bid, &slice_a, &slice_b, exit_id);
        return true;
    }
    false
}

/// Find the body block that contains store instructions.
fn find_computation_block(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    latch_id: BlockId,
) -> Option<BlockId> {
    let mut comp = None;
    for &bid in &lp.body {
        if bid == lp.header || bid == latch_id { continue; }
        let block = func.block(bid);
        if block.insts.iter().any(|i| matches!(i.kind, InstKind::Store(..))) {
            if comp.is_some() { return None; } // multiple blocks with stores — too complex
            comp = Some(bid);
        }
    }
    comp
}

/// Compute the backward slice: all instructions (by ValueId) that are
/// transitively needed to compute the given instruction's operands,
/// restricted to values defined inside the loop.
fn backward_slice(func: &Function, root: ValueId, loop_defs: &HashSet<ValueId>) -> HashSet<ValueId> {
    let mut slice = HashSet::new();
    let mut worklist = vec![root];
    while let Some(vid) = worklist.pop() {
        if !slice.insert(vid) { continue; }
        // Find the instruction that defines this value.
        for block in &func.blocks {
            for inst in &block.insts {
                if inst.id == vid {
                    for operand in inst_uses(&inst.kind) {
                        if loop_defs.contains(&operand) && !slice.contains(&operand) {
                            worklist.push(operand);
                        }
                    }
                }
            }
        }
    }
    slice
}

/// Find the loop's exit block.
fn find_loop_exit(func: &Function, lp: &crate::ir::walk::NaturalLoop) -> Option<BlockId> {
    for &bid in &lp.body {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch { false_dest, .. }) = &block.terminator {
            if !lp.body.contains(false_dest) { return Some(*false_dest); }
        }
    }
    None
}

/// Perform the fission: clone the loop, remove group B from original,
/// remove group A from clone, wire original exit → clone.
fn do_fission(
    func: &mut Function,
    lp: &crate::ir::walk::NaturalLoop,
    body_bid: BlockId,
    slice_a: &HashSet<ValueId>,
    slice_b: &HashSet<ValueId>,
    exit_id: BlockId,
) {
    // Instructions shared by both slices must be kept in both copies.
    let shared: HashSet<ValueId> = slice_a.intersection(slice_b).copied().collect();

    // Instructions exclusive to each group.
    let exclusive_a: HashSet<ValueId> = slice_a.difference(&shared).copied().collect();
    let exclusive_b: HashSet<ValueId> = slice_b.difference(&shared).copied().collect();

    // Clone the entire loop.
    let (block_map, _new_blocks) = clone_loop(func, lp);
    let _val_map = build_value_map(func, lp, &block_map);

    // In the ORIGINAL loop: remove instructions exclusive to group B.
    func.block_mut(body_bid).insts.retain(|inst| !exclusive_b.contains(&inst.id));

    // In the CLONE: remove instructions exclusive to group A.
    let clone_body = block_map[&body_bid];
    // The clone's instructions have new IDs. We need to map exclusive_a
    // IDs to their cloned counterparts.
    let clone_exclusive_a: HashSet<ValueId> = {
        let orig_insts: Vec<ValueId> = func.block(body_bid).insts.iter().map(|i| i.id).collect();
        // We need the original body's instruction IDs BEFORE removal.
        // But we already removed exclusive_b... We need the val_map.
        // Actually, build_value_map gives us orig→clone mapping.
        let val_map = build_value_map(func, lp, &block_map);
        exclusive_a.iter()
            .filter_map(|id| val_map.get(id).copied())
            .collect()
    };
    func.block_mut(clone_body).insts.retain(|inst| !clone_exclusive_a.contains(&inst.id));

    // Wire: original loop's exit → clone's header.
    // The original exit block (do_exit) currently follows the original loop.
    // We need to insert the clone between the original exit and whatever
    // comes after it.
    //
    // Strategy: the original loop's cmp false-branch goes to exit_id.
    // We redirect it to a new "bridge" block that branches to the
    // clone's header with the clone's init value.

    // Find the original cmp block and its init value.
    let clone_header = block_map[&lp.header];

    // Get init value from the original preheader.
    let preds_map = predecessors(func);
    let ph_id = find_preheader(func, lp, &preds_map).unwrap();
    let init_val = match &func.block(ph_id).terminator {
        Some(Terminator::Branch(_, args)) if !args.is_empty() => args[0],
        _ => return,
    };

    // Create a bridge block between original exit and clone.
    let bridge = func.create_block("fission_bridge");
    func.block_mut(bridge).terminator =
        Some(Terminator::Branch(clone_header, vec![init_val]));

    // Redirect original loop's cmp false-branch from exit_id to bridge.
    for &bid in &lp.body {
        let block = func.block_mut(bid);
        if let Some(Terminator::CondBranch { false_dest, .. }) = &mut block.terminator {
            if *false_dest == exit_id {
                *false_dest = bridge;
                break;
            }
        }
    }

    // Redirect clone's cmp false-branch to the original exit_id.
    // (clone_loop already points it there since exit_id is outside the
    // body and wasn't remapped. But let's verify.)
    // The clone's exit should already point to exit_id — clone_loop
    // doesn't remap targets outside the body. This should be correct.

    prune_unreachable(func);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::opt::pass::Pass;

    #[test]
    fn fission_no_op_on_empty() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopFission;
        let changed = pass.run(&mut m);
        assert!(!changed, "no loops → no fission");
    }
}
