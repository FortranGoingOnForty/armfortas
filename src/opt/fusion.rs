//! Loop fusion pass.
//!
//! Merges two adjacent loops with identical iteration spaces using
//! the LLVM-inspired "latch redirect" pattern:
//!   1. Redirect A's latch to B's body (instead of A's header)
//!   2. Redirect B's latch back to A's header (instead of B's header)
//!   3. Remove B's header/cmp blocks (now unreachable)
//!   4. Remap B's IV to A's IV
//!
//! This avoids splicing instructions between blocks entirely.

use super::dep_analysis;
use super::loop_utils::remap_inst_kind;
use super::loop_utils::{find_preheader, resolve_const_int};
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};
use std::collections::HashSet;

pub struct LoopFusion;

impl Pass for LoopFusion {
    fn name(&self) -> &'static str {
        "loop-fusion"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        let layout = module.layout;
        for func in &mut module.functions {
            if fusion_in_function(func, layout) {
                changed = true;
            }
        }
        changed
    }
}

fn fusion_in_function(func: &mut Function, layout: crate::target::TargetLayout) -> bool {
    let loops = find_natural_loops(func);
    if loops.len() < 2 {
        return false;
    }
    let ordered_bodies: Vec<Vec<BlockId>> = loops.iter().map(ordered_loop_body).collect();
    let preds = predecessors(func);

    for i in 0..loops.len() {
        for j in (i + 1)..loops.len() {
            let lp_a = &loops[i];
            let lp_b = &loops[j];
            let blocks_a = &ordered_bodies[i];
            let blocks_b = &ordered_bodies[j];

            // Both need preheaders and single latches.
            let Some(_ph_a) = find_preheader(func, lp_a, &preds) else {
                continue;
            };
            let Some(ph_b) = find_preheader(func, lp_b, &preds) else {
                continue;
            };
            if lp_a.latches.len() != 1 || lp_b.latches.len() != 1 {
                continue;
            }
            let latch_a = lp_a.latches[0];
            let latch_b = lp_b.latches[0];

            // Both headers must have exactly 1 block param (IV).
            let hdr_a = func.block(lp_a.header);
            let hdr_b = func.block(lp_b.header);
            if hdr_a.params.len() != 1 || hdr_b.params.len() != 1 {
                continue;
            }
            let iv_a = hdr_a.params[0].id;
            let iv_b = hdr_b.params[0].id;

            // Neither loop should be nested inside the other.
            if lp_a.body.iter().any(|b| lp_b.body.contains(b)) {
                continue;
            }
            if lp_b.body.iter().any(|b| lp_a.body.contains(b)) {
                continue;
            }

            // Neither loop should contain inner loops. Fusing loops
            // with nested inner loops requires more complex handling
            // (inner loop blocks need reparenting). V1: simple only.
            let has_inner_a = loops
                .iter()
                .any(|other| other.header != lp_a.header && lp_a.body.is_superset(&other.body));
            let has_inner_b = loops
                .iter()
                .any(|other| other.header != lp_b.header && lp_b.body.is_superset(&other.body));
            if has_inner_a || has_inner_b {
                continue;
            }

            // Skip loops produced by fission in the same pipeline
            // iteration — they have clone/bridge blocks that fusion
            // can't handle safely.
            let has_clone_a = blocks_a.iter().any(|&b| {
                func.block(b).name.contains("clone") || func.block(b).name.contains("fission")
            });
            let has_clone_b = blocks_b.iter().any(|&b| {
                func.block(b).name.contains("clone") || func.block(b).name.contains("fission")
            });
            if has_clone_a || has_clone_b {
                continue;
            }

            // Loop A's exit must be (or flow to) loop B's preheader.
            let exit_a = find_loop_exit(func, lp_a, blocks_a);
            let Some(exit_a) = exit_a else { continue };
            if exit_a != ph_b && !flows_to(func, exit_a, ph_b) {
                continue;
            }

            // Matching iteration spaces.
            let Some(init_a) = get_init_const(func, lp_a, &preds) else {
                continue;
            };
            let Some(init_b) = get_init_const(func, lp_b, &preds) else {
                continue;
            };
            if init_a != init_b {
                continue;
            }

            let Some(bound_a) = find_bound_const(func, blocks_a, iv_a) else {
                continue;
            };
            let Some(bound_b) = find_bound_const(func, blocks_b, iv_b) else {
                continue;
            };
            if bound_a != bound_b {
                continue;
            }

            // Dep analysis: fusion legal?
            if !dep_analysis::fusion_legal(func, &lp_a.body, &lp_b.body, iv_a, iv_b, layout) {
                continue;
            }

            let body_a = find_body_block(func, lp_a, blocks_a, latch_a);
            let Some(body_a_id) = body_a else { continue };

            // Find B's body block (the one with stores/computation).
            let body_b = find_body_block(func, lp_b, blocks_b, latch_b);
            let Some(body_b_id) = body_b else { continue };

            let cmp_a = find_cmp_block(func, blocks_a);
            let Some(cmp_a_id) = cmp_a else { continue };

            // Find B's cmp block (the one with ICmp).
            let cmp_b = find_cmp_block(func, blocks_b);
            let Some(cmp_b_id) = cmp_b else { continue };

            if !has_simple_fusion_shape(func, lp_a, latch_a, body_a_id, cmp_a_id) {
                continue;
            }
            if !has_simple_fusion_shape(func, lp_b, latch_b, body_b_id, cmp_b_id) {
                continue;
            }

            // Find B's exit block.
            let exit_b = find_loop_exit(func, lp_b, blocks_b);
            let Some(exit_b) = exit_b else { continue };

            // ---- Perform fusion via latch redirect ----
            do_fusion_latch_redirect(
                func, lp_b, blocks_a, blocks_b, latch_a, latch_b, iv_a, iv_b, body_b_id, cmp_b_id,
                exit_a, exit_b,
            );
            return true;
        }
    }
    false
}

/// Fuse by redirecting latches:
///   A's latch → B's body (skipping B's header/cmp)
///   B's latch → A's header
///   Remap iv_b → iv_a throughout B's body
fn do_fusion_latch_redirect(
    func: &mut Function,
    lp_b: &crate::ir::walk::NaturalLoop,
    blocks_a: &[BlockId],
    blocks_b: &[BlockId],
    latch_a: BlockId,
    latch_b: BlockId,
    iv_a: ValueId,
    iv_b: ValueId,
    body_b_id: BlockId,
    _cmp_b_id: BlockId,
    exit_a: BlockId,
    exit_b: BlockId,
) {
    // Step 1: Remap iv_b → iv_a throughout ALL of B's body blocks.
    // We rebuild each instruction with the IV substitution.
    let mut sub_map = std::collections::HashMap::new();
    sub_map.insert(iv_b, iv_a);
    for &bid in blocks_b {
        let old_insts: Vec<Inst> = func.block(bid).insts.clone();
        let new_insts: Vec<Inst> = old_insts
            .into_iter()
            .map(|inst| {
                let new_kind = remap_inst_kind(&inst.kind, &sub_map);
                Inst {
                    kind: new_kind,
                    ..inst
                }
            })
            .collect();
        func.block_mut(bid).insts = new_insts;
        // Also remap terminator operands.
        let old_term = func.block(bid).terminator.clone();
        if let Some(term) = old_term {
            let new_term = super::loop_utils::remap_terminator(
                &term,
                &std::collections::HashMap::new(), // no block remapping
                &sub_map,
            );
            func.block_mut(bid).terminator = Some(new_term);
        }
    }

    // Step 1.5: Clone the gap block's (exit_a's) instructions into B's
    // body block. These are constants (like `%24 = const 1`) that B's
    // body needs but that will be destroyed when we replace exit_a's
    // content with B's exit block in step 3. Prepend them to B's body
    // so they dominate all uses within B's body.
    let gap_insts: Vec<Inst> = func.block(exit_a).insts.clone();
    let existing_b_insts: Vec<Inst> = func.block(body_b_id).insts.clone();
    func.block_mut(body_b_id).insts = gap_insts;
    func.block_mut(body_b_id).insts.extend(existing_b_insts);

    // Step 2: A's latch currently branches to A's header. Redirect it
    // to B's body block instead. This makes the fused loop run A's body
    // then B's body on each iteration.
    //
    // But we need to preserve the IV increment: A's latch computes
    // iv_next and passes it to A's header. After fusion, B's body
    // should see iv_a (current iteration), and B's latch should pass
    // iv_next back to A's header.
    //
    // The cleanest approach: redirect A's body exit (the block that
    // branches to A's latch) to branch to B's body instead. Then B's
    // body branches to B's latch, and B's latch branches to A's latch,
    // and A's latch branches to A's header with iv_next.
    //
    // Actually simpler: redirect A's latch to branch through B's body
    // before going back to A's header.
    //
    // The issue is that A's latch has the IV increment instruction.
    // We want: A's body → B's body → A's latch (increment) → A's header.
    //
    // So redirect the block that branches to A's latch to go to B's body
    // instead. Then B's latch branches to A's latch (not B's header).

    // Find the block that branches to latch_a (A's body exit).
    let a_body_exit = find_branch_to(func, blocks_a, latch_a);
    let Some(a_body_exit_id) = a_body_exit else {
        return;
    };

    // Redirect A's body exit → B's body.
    redirect_branch(func, a_body_exit_id, latch_a, body_b_id);

    // Redirect B's latch → A's latch (not B's header).
    // B's latch currently: br B's_header(iv_next_b). Change to: br A's_latch.
    // But B's latch's iv_next_b should be discarded — A's latch will
    // compute iv_next_a. So B's latch should just branch to A's latch
    // with no args.
    func.block_mut(latch_b).terminator = Some(Terminator::Branch(latch_a, vec![]));

    // Step 3: A's cmp exit (exit_a) should now go to B's exit (exit_b)
    // since B's header/cmp are bypassed. Copy B's exit block content
    // into A's exit block.
    let exit_b_insts: Vec<Inst> = func.block(exit_b).insts.clone();
    let exit_b_term = func.block(exit_b).terminator.clone();
    func.block_mut(exit_a).insts = exit_b_insts;
    func.block_mut(exit_a).terminator = exit_b_term;

    // Step 4: Mark B's header, cmp, and preheader blocks as unreachable.
    func.block_mut(lp_b.header).terminator = Some(Terminator::Unreachable);
    // Mark B's cmp blocks too.
    for &bid in blocks_b {
        if bid == body_b_id || bid == latch_b {
            continue;
        }
        func.block_mut(bid).terminator = Some(Terminator::Unreachable);
    }

    crate::ir::walk::prune_unreachable(func);
}

fn find_branch_to(func: &Function, blocks: &[BlockId], target: BlockId) -> Option<BlockId> {
    for &bid in blocks {
        if bid == target {
            continue;
        }
        let block = func.block(bid);
        match &block.terminator {
            Some(Terminator::Branch(dest, _)) if *dest == target => return Some(bid),
            _ => {}
        }
    }
    None
}

fn redirect_branch(func: &mut Function, from: BlockId, old_target: BlockId, new_target: BlockId) {
    let block = func.block_mut(from);
    if let Some(Terminator::Branch(dest, args)) = &mut block.terminator {
        if *dest == old_target {
            *dest = new_target;
            args.clear(); // B's body doesn't take args from A's body
        }
    }
}

fn find_loop_exit(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    blocks: &[BlockId],
) -> Option<BlockId> {
    for &bid in blocks {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch { false_dest, .. }) = &block.terminator {
            if !lp.body.contains(false_dest) {
                return Some(*false_dest);
            }
        }
    }
    None
}

fn flows_to(func: &Function, from: BlockId, to: BlockId) -> bool {
    let block = func.block(from);
    match &block.terminator {
        Some(Terminator::Branch(dest, _)) => {
            if *dest == to {
                return true;
            }
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

fn find_bound_const(func: &Function, blocks: &[BlockId], iv: ValueId) -> Option<i64> {
    for &bid in blocks {
        let block = func.block(bid);
        for inst in &block.insts {
            if let InstKind::ICmp(_, a, b) = &inst.kind {
                let bound_val = if *a == iv {
                    *b
                } else if *b == iv {
                    *a
                } else {
                    continue;
                };
                return resolve_const_int(func, bound_val);
            }
        }
    }
    None
}

fn find_body_block(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    blocks: &[BlockId],
    latch_id: BlockId,
) -> Option<BlockId> {
    for &bid in blocks {
        if bid == lp.header || bid == latch_id {
            continue;
        }
        let block = func.block(bid);
        if block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::Store(..)))
        {
            return Some(bid);
        }
    }
    None
}

fn find_cmp_block(func: &Function, blocks: &[BlockId]) -> Option<BlockId> {
    for &bid in blocks {
        let block = func.block(bid);
        if block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::ICmp(..)))
        {
            return Some(bid);
        }
    }
    None
}

fn ordered_loop_body(lp: &crate::ir::walk::NaturalLoop) -> Vec<BlockId> {
    let mut blocks: Vec<BlockId> = lp.body.iter().copied().collect();
    blocks.sort_unstable_by_key(|bid| bid.0);
    blocks
}

fn has_simple_fusion_shape(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    latch_id: BlockId,
    body_id: BlockId,
    cmp_id: BlockId,
) -> bool {
    let allowed: HashSet<BlockId> = [lp.header, cmp_id, body_id, latch_id].into_iter().collect();
    if lp.body.iter().any(|bid| !allowed.contains(bid)) {
        return false;
    }

    let body = func.block(body_id);
    if !body.params.is_empty() {
        return false;
    }
    matches!(&body.terminator, Some(Terminator::Branch(dest, args)) if *dest == latch_id && args.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::ir::walk::NaturalLoop;
    use crate::opt::pass::Pass;

    #[test]
    fn fusion_no_op_on_empty() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopFusion;
        let changed = pass.run(&mut m);
        assert!(!changed);
    }

    #[test]
    fn loop_body_queries_use_block_id_order() {
        let forward = NaturalLoop {
            header: BlockId(2),
            body: [BlockId(9), BlockId(2), BlockId(5)].into_iter().collect(),
            latches: vec![BlockId(9)],
        };
        let reverse = NaturalLoop {
            header: BlockId(2),
            body: [BlockId(5), BlockId(2), BlockId(9)].into_iter().collect(),
            latches: vec![BlockId(9)],
        };

        let expected = vec![BlockId(2), BlockId(5), BlockId(9)];
        assert_eq!(ordered_loop_body(&forward), expected);
        assert_eq!(ordered_loop_body(&reverse), expected);
    }
}
