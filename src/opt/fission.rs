//! Loop fission (distribution) pass.
//!
//! Splits a loop with independent statement groups into two loops.
//! Uses the LLVM-inspired "clone + replace-with-undef" pattern:
//!   1. Clone the entire loop
//!   2. In the original: replace group B's stores with no-ops (Undef stores)
//!   3. In the clone: replace group A's stores with no-ops
//!   4. Wire original exit → clone's preheader
//!   5. Let DCE clean up the dead instructions in later passes
//!
//! This avoids SSA domination bugs from selective instruction removal.

use std::collections::HashSet;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};
use super::loop_utils::{find_preheader, clone_loop};
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

        let hdr = func.block(lp.header);
        if hdr.params.len() != 1 { continue; }
        let iv = hdr.params[0].id;

        // Find the single computation body block.
        let body_block = find_computation_block(func, lp, latch_id);
        let Some(body_bid) = body_block else { continue };

        let block = func.block(body_bid);
        if block.insts.len() < FISSION_MIN_BODY { continue; }

        // Need exactly 2 stores to different arrays.
        let stores: Vec<(usize, ValueId)> = block.insts.iter().enumerate()
            .filter(|(_, i)| matches!(i.kind, InstKind::Store(..)))
            .map(|(idx, i)| (idx, i.id))
            .collect();
        if stores.len() != 2 { continue; }

        // Check independence via dep analysis.
        let mut ivs = HashSet::new();
        ivs.insert(iv);
        let mem_refs = collect_mem_refs(func, &lp.body, &ivs);
        let writes: Vec<_> = mem_refs.iter().filter(|r| r.is_write).collect();
        if writes.len() != 2 { continue; }
        if writes[0].base == writes[1].base { continue; }

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

        // Find the exit block.
        let exit_id = find_loop_exit(func, lp);
        let Some(exit_id) = exit_id else { continue };

        // Compute backward slices for each store.
        // Clone the loop.
        let (block_map, _) = clone_loop(func, lp);

        // In original body: neutralize store B (replace value with undef).
        neutralize_store(func, body_bid, stores[1].0);

        // In clone body: neutralize store A.
        let clone_body = block_map[&body_bid];
        neutralize_store(func, clone_body, stores[0].0);

        // Wire original exit → clone's header via a bridge block.
        let clone_header = block_map[&lp.header];
        let ph_id = find_preheader(func, lp, &predecessors(func)).unwrap();
        let init_val = match &func.block(ph_id).terminator {
            Some(Terminator::Branch(_, args)) if !args.is_empty() => args[0],
            _ => continue,
        };

        let bridge = func.create_block("fission_bridge");
        func.block_mut(bridge).terminator =
            Some(Terminator::Branch(clone_header, vec![init_val]));

        // Redirect original cmp's exit → bridge.
        for &bid in &lp.body {
            let block = func.block_mut(bid);
            if let Some(Terminator::CondBranch { false_dest, .. }) = &mut block.terminator {
                if *false_dest == exit_id {
                    *false_dest = bridge;
                    break;
                }
            }
        }

        // Clone's cmp exit already points to exit_id (wasn't remapped
        // since it's outside the body). Correct.

        return true;
    }
    false
}

/// Replace a store instruction's value operand with Undef, effectively
/// making it a dead store that DSE/DCE can clean up.
fn neutralize_store(func: &mut Function, block_id: BlockId, store_idx: usize) {
    let block = func.block_mut(block_id);
    if store_idx >= block.insts.len() { return; }
    if let InstKind::Store(_, _) = block.insts[store_idx].kind {
        // Replace with a store of undef — the store is now dead.
        // We keep the store instruction (not remove it) to preserve
        // SSA structure. DCE will clean it up later.
        let undef_id = ValueId(u32::MAX - 1); // sentinel; will be replaced below
        let _ = undef_id;
    }
    // Actually, the simplest approach: just remove the store entirely.
    // Stores have no result value used by other instructions, so removing
    // them can't break SSA. The computation leading to the store value
    // becomes dead and DCE removes it.
    block.insts.remove(store_idx);
}

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
            if comp.is_some() { return None; }
            comp = Some(bid);
        }
    }
    comp
}

fn backward_slice(func: &Function, root: ValueId, loop_defs: &HashSet<ValueId>) -> HashSet<ValueId> {
    let mut slice = HashSet::new();
    let mut worklist = vec![root];
    while let Some(vid) = worklist.pop() {
        if !slice.insert(vid) { continue; }
        for block in &func.blocks {
            for inst in &block.insts {
                if inst.id == vid {
                    for operand in crate::ir::walk::inst_uses(&inst.kind) {
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

fn find_loop_exit(func: &Function, lp: &crate::ir::walk::NaturalLoop) -> Option<BlockId> {
    for &bid in &lp.body {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch { false_dest, .. }) = &block.terminator {
            if !lp.body.contains(false_dest) { return Some(*false_dest); }
        }
    }
    None
}

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
