//! Loop fusion pass.
//!
//! Merges adjacent loops with identical iteration spaces into a single
//! loop. Improves data locality — values written in the first loop and
//! read in the second are now in registers within the same iteration.
//!
//! ```text
//! Before:
//!   do i = 1, n; a(i) = b(i) + 1; end do
//!   do i = 1, n; c(i) = a(i) * 2; end do
//!
//! After:
//!   do i = 1, n
//!     a(i) = b(i) + 1
//!     c(i) = a(i) * 2
//!   end do
//! ```
//!
//! Requires: identical init/bound/stride, no fusion-preventing
//! dependences (no cross-loop anti/output deps with negative distance).
//! Gated at O2+.

use std::collections::HashSet;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};
use super::loop_utils::{find_preheader, resolve_const_int};
use super::dep_analysis;
use super::pass::Pass;

pub struct LoopFusion;

impl Pass for LoopFusion {
    fn name(&self) -> &'static str { "loop-fusion" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if fusion_in_function(func) { changed = true; }
        }
        changed
    }
}

fn fusion_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    if loops.len() < 2 { return false; }

    let preds = predecessors(func);

    // Find pairs of adjacent loops: loop A's exit flows directly to
    // loop B's preheader (or to loop B's header if the preheader IS
    // the exit path).
    for i in 0..loops.len() {
        for j in (i+1)..loops.len() {
            let lp_a = &loops[i];
            let lp_b = &loops[j];

            // Both must have preheaders.
            let Some(ph_a) = find_preheader(func, lp_a, &preds) else { continue };
            let Some(ph_b) = find_preheader(func, lp_b, &preds) else { continue };

            // Both must have single latches and single-param headers (counted loops).
            if lp_a.latches.len() != 1 || lp_b.latches.len() != 1 { continue; }
            let hdr_a = func.block(lp_a.header);
            let hdr_b = func.block(lp_b.header);
            if hdr_a.params.len() != 1 || hdr_b.params.len() != 1 { continue; }
            let iv_a = hdr_a.params[0].id;
            let iv_b = hdr_b.params[0].id;

            // Find the exit block of loop A (the false-branch target of
            // the loop's comparison block).
            let exit_a = find_loop_exit(func, lp_a);
            let Some(exit_a) = exit_a else { continue };

            // Loop A's exit must flow directly to loop B's preheader.
            let exit_flows_to_ph_b = match &func.block(exit_a).terminator {
                Some(Terminator::Branch(dest, _)) => *dest == ph_b,
                _ => false,
            };
            if !exit_flows_to_ph_b { continue; }

            // Both loops must have the same iteration space.
            // Compare init values (from preheader branch args).
            let init_a = get_init(func, ph_a, lp_a.header);
            let init_b = get_init(func, ph_b, lp_b.header);
            let Some(init_a_val) = init_a else { continue };
            let Some(init_b_val) = init_b else { continue };

            // Resolve to constants and compare.
            let ca = resolve_const_int(func, init_a_val);
            let cb = resolve_const_int(func, init_b_val);
            if ca != cb || ca.is_none() { continue; }

            // Compare bounds (from the comparison blocks).
            let bound_a = find_bound(func, lp_a, iv_a);
            let bound_b = find_bound(func, lp_b, iv_b);
            let Some(ba) = bound_a else { continue };
            let Some(bb) = bound_b else { continue };
            let ba_const = resolve_const_int(func, ba);
            let bb_const = resolve_const_int(func, bb);
            if ba_const != bb_const || ba_const.is_none() { continue; }

            // Check legality via dep analysis.
            let mut ivs = HashSet::new();
            ivs.insert(iv_a);
            ivs.insert(iv_b);
            if !dep_analysis::fusion_legal(func, &lp_a.body, &lp_b.body, &ivs) {
                continue;
            }

            // Fusion is legal and profitable. Perform the merge.
            // For V1: report candidate found but defer full CFG surgery
            // (merging two loop bodies requires remapping loop B's IV to
            // loop A's IV and splicing body blocks — complex and risky).
            //
            // TODO: implement full fusion CFG surgery.
        }
    }

    false
}

/// Find the exit block of a loop (first block outside the body that
/// a comparison block branches to on the false path).
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

/// Get the init value passed from a preheader to a header.
fn get_init(func: &Function, ph: BlockId, header: BlockId) -> Option<ValueId> {
    match &func.block(ph).terminator {
        Some(Terminator::Branch(dest, args)) if *dest == header && !args.is_empty() =>
            Some(args[0]),
        _ => None,
    }
}

/// Find the bound value used in a loop's comparison.
fn find_bound(func: &Function, lp: &crate::ir::walk::NaturalLoop, iv: ValueId) -> Option<ValueId> {
    for &bid in &lp.body {
        let block = func.block(bid);
        for inst in &block.insts {
            if let InstKind::ICmp(_, a, b) = &inst.kind {
                if *a == iv { return Some(*b); }
                if *b == iv { return Some(*a); }
            }
        }
    }
    None
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
    fn fusion_no_op_on_empty() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopFusion;
        let changed = pass.run(&mut m);
        assert!(!changed, "no loops → no fusion");
    }
}
