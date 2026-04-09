//! Loop fusion pass.
//!
//! Merges two adjacent loops with identical iteration spaces into a
//! single loop. The second loop's body instructions are spliced into
//! the first loop's body with the second IV remapped to the first.
//!
//! ```text
//! Before:
//!   do i = 1, n; a(i) = i * 2; end do
//!   do i = 1, n; b(i) = a(i) + 1; end do
//!
//! After:
//!   do i = 1, n; a(i) = i * 2; b(i) = a(i) + 1; end do
//! ```
//!
//! Requires: identical init/bound/stride, no fusion-preventing deps.

use std::collections::HashSet;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors, prune_unreachable};
use super::loop_utils::{find_preheader, resolve_const_int, remap_inst_kind};
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

    // Try all pairs. We process one fusion per call and let the
    // pass manager's fixpoint loop handle cascading opportunities.
    for i in 0..loops.len() {
        for j in (i+1)..loops.len() {
            let lp_a = &loops[i];
            let lp_b = &loops[j];

            // Both need preheaders and single latches.
            let Some(_ph_a) = find_preheader(func, lp_a, &preds) else { continue };
            let Some(ph_b) = find_preheader(func, lp_b, &preds) else { continue };
            if lp_a.latches.len() != 1 || lp_b.latches.len() != 1 { continue; }

            // Both headers must have exactly 1 block param (counted loop IV).
            let hdr_a = func.block(lp_a.header);
            let hdr_b = func.block(lp_b.header);
            if hdr_a.params.len() != 1 || hdr_b.params.len() != 1 { continue; }
            let iv_a = hdr_a.params[0].id;
            let iv_b = hdr_b.params[0].id;

            // Loop A's exit must flow directly to loop B's preheader.
            let exit_a = find_loop_exit(func, lp_a);
            let Some(exit_a) = exit_a else { continue };

            // The exit block must branch (possibly through a chain) to ph_b.
            if !flows_to(func, exit_a, ph_b) { continue; }

            // Matching iteration spaces: same init, bound, stride constants.
            let Some(init_a) = get_init_const(func, lp_a, &preds) else { continue };
            let Some(init_b) = get_init_const(func, lp_b, &preds) else { continue };
            if init_a != init_b { continue; }

            let Some(bound_a) = find_bound_const(func, lp_a, iv_a) else { continue };
            let Some(bound_b) = find_bound_const(func, lp_b, iv_b) else { continue };
            if bound_a != bound_b { continue; }

            // Check legality via dep analysis.
            let mut ivs = HashSet::new();
            ivs.insert(iv_a);
            ivs.insert(iv_b);
            if !dep_analysis::fusion_legal(func, &lp_a.body, &lp_b.body, &ivs) {
                continue;
            }

            // Find the body blocks of each loop (blocks with stores).
            let body_a = find_body_block(func, lp_a, lp_a.latches[0]);
            let body_b = find_body_block(func, lp_b, lp_b.latches[0]);
            let Some(body_a_id) = body_a else { continue };
            let Some(body_b_id) = body_b else { continue };

            // Find loop B's exit block.
            let exit_b = find_loop_exit(func, lp_b);
            let Some(exit_b) = exit_b else { continue };

            // ---- Perform fusion ----
            do_fusion(func, lp_a, lp_b, body_a_id, body_b_id,
                      iv_a, iv_b, exit_a, exit_b);
            return true;
        }
    }
    false
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

fn flows_to(func: &Function, from: BlockId, to: BlockId) -> bool {
    // Direct branch or through a short chain.
    let block = func.block(from);
    match &block.terminator {
        Some(Terminator::Branch(dest, _)) => {
            if *dest == to { return true; }
            // One level of indirection.
            let mid = func.block(*dest);
            if let Some(Terminator::Branch(dest2, _)) = &mid.terminator {
                return *dest2 == to;
            }
            false
        }
        _ => false,
    }
}

fn get_init_const(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    preds: &std::collections::HashMap<BlockId, Vec<BlockId>>,
) -> Option<i64> {
    let ph = find_preheader(func, lp, preds)?;
    let init_val = match &func.block(ph).terminator {
        Some(Terminator::Branch(_, args)) if !args.is_empty() => args[0],
        _ => return None,
    };
    resolve_const_int(func, init_val)
}

fn find_bound_const(func: &Function, lp: &crate::ir::walk::NaturalLoop, iv: ValueId) -> Option<i64> {
    for &bid in &lp.body {
        let block = func.block(bid);
        for inst in &block.insts {
            if let InstKind::ICmp(_, a, b) = &inst.kind {
                let bound_val = if *a == iv { *b } else if *b == iv { *a } else { continue };
                return resolve_const_int(func, bound_val);
            }
        }
    }
    None
}

fn find_body_block(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    latch_id: BlockId,
) -> Option<BlockId> {
    for &bid in &lp.body {
        if bid == lp.header || bid == latch_id { continue; }
        let block = func.block(bid);
        if block.insts.iter().any(|i| matches!(i.kind, InstKind::Store(..))) {
            return Some(bid);
        }
    }
    None
}

/// Fuse loop B into loop A by splicing B's body instructions into A's body.
fn do_fusion(
    func: &mut Function,
    lp_a: &crate::ir::walk::NaturalLoop,
    lp_b: &crate::ir::walk::NaturalLoop,
    body_a_id: BlockId,
    body_b_id: BlockId,
    iv_a: ValueId,
    iv_b: ValueId,
    exit_a: BlockId,
    exit_b: BlockId,
) {
    // Build remap: iv_b → iv_a, and new IDs for all cloned instructions.
    let mut val_map: std::collections::HashMap<ValueId, ValueId> = std::collections::HashMap::new();
    val_map.insert(iv_b, iv_a);

    // Clone B's body instructions with remapped operands.
    let b_insts: Vec<Inst> = func.block(body_b_id).insts.clone();
    let mut new_insts = Vec::new();
    for inst in &b_insts {
        let new_id = func.next_value_id();
        func.register_type(new_id, inst.ty.clone());
        val_map.insert(inst.id, new_id);
        let new_kind = remap_inst_kind(&inst.kind, &val_map);
        new_insts.push(Inst {
            id: new_id,
            kind: new_kind,
            ty: inst.ty.clone(),
            span: inst.span,
        });
    }

    // Append the cloned instructions to A's body block.
    func.block_mut(body_a_id).insts.extend(new_insts);

    // Redirect loop A's exit to where loop B's exit goes.
    // Loop A's exit block currently branches to loop B's preheader.
    // After fusion, it should go directly to where loop B's exit goes.
    let exit_b_term = func.block(exit_b).terminator.clone();
    func.block_mut(exit_a).terminator = exit_b_term;

    // Mark loop B's blocks as unreachable.
    for &bid in &lp_b.body {
        func.block_mut(bid).terminator = Some(Terminator::Unreachable);
    }

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
