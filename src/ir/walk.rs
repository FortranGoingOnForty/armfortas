//! Shared IR-walking helpers.
//!
//! This module is the **single canonical source** for operand
//! enumeration, terminator queries, and small CFG utilities used by
//! both the verifier and the optimizer. Audit B-6: prior to this file
//! the same machinery was duplicated five ways across `verify.rs`,
//! `opt/util.rs`, `opt/cse.rs`, `opt/dce.rs`, and `opt/const_prop.rs`,
//! creating a maintenance footgun — adding a new `InstKind` variant
//! required updating every duplicate, and missing one would silently
//! break that pass.
//!
//! Anything operating on `InstKind` or `Terminator` at the level of
//! "what operands does this instruction read?" or "where does this
//! terminator branch?" should live here.

use super::inst::*;
use std::collections::{HashMap, HashSet, VecDeque};

// =====================================================================
//                     Operand enumeration (read-only)
// =====================================================================

/// All `ValueId`s consumed as operands by an instruction.
pub fn inst_uses(kind: &InstKind) -> Vec<ValueId> {
    match kind {
        InstKind::ConstInt(..) | InstKind::ConstFloat(..) |
        InstKind::ConstBool(..) | InstKind::ConstString(..) |
        InstKind::Undef(..) | InstKind::Alloca(..) => vec![],

        InstKind::IAdd(a, b) | InstKind::ISub(a, b) |
        InstKind::IMul(a, b) | InstKind::IDiv(a, b) |
        InstKind::IMod(a, b) => vec![*a, *b],
        InstKind::INeg(a) => vec![*a],

        InstKind::FAdd(a, b) | InstKind::FSub(a, b) |
        InstKind::FMul(a, b) | InstKind::FDiv(a, b) |
        InstKind::FPow(a, b) => vec![*a, *b],
        InstKind::FNeg(a) | InstKind::FAbs(a) | InstKind::FSqrt(a) => vec![*a],

        InstKind::ICmp(_, a, b) | InstKind::FCmp(_, a, b) => vec![*a, *b],

        InstKind::And(a, b) | InstKind::Or(a, b) => vec![*a, *b],
        InstKind::Not(a) => vec![*a],

        InstKind::Select(c, t, f) => vec![*c, *t, *f],

        InstKind::BitAnd(a, b) | InstKind::BitOr(a, b) |
        InstKind::BitXor(a, b) | InstKind::Shl(a, b) |
        InstKind::LShr(a, b) | InstKind::AShr(a, b) => vec![*a, *b],
        InstKind::BitNot(a) | InstKind::CountLeadingZeros(a) |
        InstKind::CountTrailingZeros(a) | InstKind::PopCount(a) => vec![*a],

        InstKind::IntToFloat(v, _) | InstKind::FloatToInt(v, _) |
        InstKind::FloatExtend(v, _) | InstKind::FloatTrunc(v, _) |
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) => vec![*v],

        InstKind::Load(a) => vec![*a],
        InstKind::Store(v, a) => vec![*v, *a],
        InstKind::GetElementPtr(base, idxs) => {
            let mut uses = vec![*base];
            uses.extend(idxs);
            uses
        }

        InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => args.clone(),

        InstKind::ExtractField(agg, _) => vec![*agg],
        InstKind::InsertField(agg, _, val) => vec![*agg, *val],
    }
}

/// All `ValueId`s consumed by a terminator.
pub fn terminator_uses(term: &Terminator) -> Vec<ValueId> {
    match term {
        Terminator::Return(None) | Terminator::Unreachable => vec![],
        Terminator::Return(Some(v)) => vec![*v],
        Terminator::Branch(_, args) => args.clone(),
        Terminator::CondBranch { cond, true_args, false_args, .. } => {
            let mut uses = vec![*cond];
            uses.extend(true_args);
            uses.extend(false_args);
            uses
        }
        Terminator::Switch { selector, .. } => vec![*selector],
    }
}

/// All successor `BlockId`s of a terminator.
pub fn terminator_targets(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Return(_) | Terminator::Unreachable => vec![],
        Terminator::Branch(d, _) => vec![*d],
        Terminator::CondBranch { true_dest, false_dest, .. } => vec![*true_dest, *false_dest],
        Terminator::Switch { cases, default, .. } => {
            let mut t: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            t.push(*default);
            t
        }
    }
}

// =====================================================================
//                       Operand enumeration (mutable)
// =====================================================================

/// Apply a closure to every operand slot of an instruction in place.
pub fn for_each_operand_mut(kind: &mut InstKind, mut r: impl FnMut(&mut ValueId)) {
    match kind {
        InstKind::ConstInt(..) | InstKind::ConstFloat(..) |
        InstKind::ConstBool(..) | InstKind::ConstString(..) |
        InstKind::Undef(..) | InstKind::Alloca(..) => {}

        InstKind::IAdd(a, b) | InstKind::ISub(a, b) |
        InstKind::IMul(a, b) | InstKind::IDiv(a, b) |
        InstKind::IMod(a, b) => { r(a); r(b); }
        InstKind::INeg(a) => r(a),

        InstKind::FAdd(a, b) | InstKind::FSub(a, b) |
        InstKind::FMul(a, b) | InstKind::FDiv(a, b) |
        InstKind::FPow(a, b) => { r(a); r(b); }
        InstKind::FNeg(a) | InstKind::FAbs(a) | InstKind::FSqrt(a) => r(a),

        InstKind::ICmp(_, a, b) | InstKind::FCmp(_, a, b) => { r(a); r(b); }

        InstKind::And(a, b) | InstKind::Or(a, b) => { r(a); r(b); }
        InstKind::Not(a) => r(a),

        InstKind::Select(c, t, f) => { r(c); r(t); r(f); }

        InstKind::BitAnd(a, b) | InstKind::BitOr(a, b) |
        InstKind::BitXor(a, b) | InstKind::Shl(a, b) |
        InstKind::LShr(a, b) | InstKind::AShr(a, b) => { r(a); r(b); }
        InstKind::BitNot(a) | InstKind::CountLeadingZeros(a) |
        InstKind::CountTrailingZeros(a) | InstKind::PopCount(a) => r(a),

        InstKind::IntToFloat(v, _) | InstKind::FloatToInt(v, _) |
        InstKind::FloatExtend(v, _) | InstKind::FloatTrunc(v, _) |
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) => r(v),

        InstKind::Load(a) => r(a),
        InstKind::Store(v, a) => { r(v); r(a); }
        InstKind::GetElementPtr(base, idxs) => {
            r(base);
            for i in idxs { r(i); }
        }

        InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
            for a in args { r(a); }
        }

        InstKind::ExtractField(agg, _) => r(agg),
        InstKind::InsertField(agg, _, val) => { r(agg); r(val); }
    }
}

/// Apply a closure to every operand slot of a terminator in place.
pub fn for_each_terminator_operand_mut(
    term: &mut Terminator,
    mut r: impl FnMut(&mut ValueId),
) {
    match term {
        Terminator::Return(None) | Terminator::Unreachable => {}
        Terminator::Return(Some(v)) => r(v),
        Terminator::Branch(_, args) => for a in args { r(a); },
        Terminator::CondBranch { cond, true_args, false_args, .. } => {
            r(cond);
            for a in true_args { r(a); }
            for a in false_args { r(a); }
        }
        Terminator::Switch { selector, .. } => r(selector),
    }
}

/// Replace every use of `old` with `new` across the entire function.
/// Definitions are unaffected — only operand slots in instructions and
/// terminators are rewritten.
pub fn substitute_uses(func: &mut Function, old: ValueId, new: ValueId) {
    let r = |v: &mut ValueId| if *v == old { *v = new; };
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            for_each_operand_mut(&mut inst.kind, r);
        }
        if let Some(term) = &mut block.terminator {
            for_each_terminator_operand_mut(term, r);
        }
    }
}

// =====================================================================
//                              CFG queries
// =====================================================================

/// Build a predecessor map: for each block, the list of blocks that
/// branch into it.
pub fn predecessors(func: &Function) -> HashMap<BlockId, Vec<BlockId>> {
    let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for block in &func.blocks {
        preds.entry(block.id).or_default();
    }
    for block in &func.blocks {
        if let Some(term) = &block.terminator {
            for tgt in terminator_targets(term) {
                preds.entry(tgt).or_default().push(block.id);
            }
        }
    }
    preds
}

/// Compute the dominator set for each block via iterative dataflow.
/// Result: `dom[B]` is the set of blocks that dominate `B` (including
/// `B` itself).
///
/// Audit M4-3: blocks that are **not reachable from the entry** are
/// assigned the empty set (they dominate nothing and nothing dominates
/// them). The earlier version left unreachable blocks at the initial
/// `{all_blocks}` default, which made every edge inside an unreachable
/// component look like a back-edge (because the source "dominated"
/// the target by default). That in turn let `find_natural_loops`
/// fabricate phantom loops in unreachable code. LICM was shielded by
/// the N-6 pre-prune, but any future consumer of `compute_dominators`
/// would trip over the latent bug.
pub fn compute_dominators(func: &Function) -> HashMap<BlockId, HashSet<BlockId>> {
    let all_blocks: HashSet<BlockId> = func.blocks.iter().map(|b| b.id).collect();

    // First, compute the set of blocks reachable from the entry via
    // a forward BFS over terminator targets.
    let mut reachable: HashSet<BlockId> = HashSet::new();
    let mut queue: VecDeque<BlockId> = VecDeque::new();
    queue.push_back(func.entry);
    reachable.insert(func.entry);
    while let Some(bid) = queue.pop_front() {
        if let Some(block) = func.blocks.iter().find(|b| b.id == bid) {
            if let Some(term) = &block.terminator {
                for tgt in terminator_targets(term) {
                    if reachable.insert(tgt) {
                        queue.push_back(tgt);
                    }
                }
            }
        }
    }

    let mut doms: HashMap<BlockId, HashSet<BlockId>> = HashMap::new();

    // Entry is dominated only by itself.
    let mut entry_set = HashSet::new();
    entry_set.insert(func.entry);
    doms.insert(func.entry, entry_set);

    // Reachable non-entry blocks: initialize to the universe of
    // reachable blocks (so the intersection converges).
    // Unreachable blocks: initialize to the empty set and never
    // update them — they participate in no meaningful dominance
    // relationship.
    for block in &func.blocks {
        if block.id == func.entry { continue; }
        if reachable.contains(&block.id) {
            doms.insert(block.id, reachable.clone());
        } else {
            doms.insert(block.id, HashSet::new());
        }
    }

    let preds = predecessors(func);
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            if block.id == func.entry { continue; }
            if !reachable.contains(&block.id) { continue; }
            let plist = preds.get(&block.id).cloned().unwrap_or_default();
            // Reachable-only predecessors — an edge from an
            // unreachable block doesn't contribute to dominance.
            let reachable_preds: Vec<BlockId> = plist.into_iter()
                .filter(|p| reachable.contains(p))
                .collect();
            if reachable_preds.is_empty() { continue; }
            let mut new_dom = reachable.clone();
            for p in &reachable_preds {
                if let Some(pd) = doms.get(p) {
                    new_dom = new_dom.intersection(pd).copied().collect();
                }
            }
            new_dom.insert(block.id);
            if doms.get(&block.id) != Some(&new_dom) {
                doms.insert(block.id, new_dom);
                changed = true;
            }
        }
    }
    // Silence unused-var lint for `all_blocks` — kept for potential
    // future needs (and because removing it would break callers that
    // expect every block to have an entry in the map).
    let _ = all_blocks;
    doms
}

/// A natural loop: header + the set of blocks in the loop body.
#[derive(Debug, Clone)]
pub struct NaturalLoop {
    /// The unique entry point of the loop.
    pub header: BlockId,
    /// All blocks belonging to the loop body, including the header.
    pub body: HashSet<BlockId>,
    /// Blocks containing the back-edges into the header (always a
    /// subset of `body`).
    pub latches: Vec<BlockId>,
}

/// Discover natural loops in a function. A natural loop is identified
/// by a back edge `(B → H)` where `H` dominates `B`. The body is the
/// set of nodes that can reach `B` without going through `H`, plus `H`
/// and `B` themselves.
///
/// We do not collapse multiple back-edges into the same header into a
/// single loop here — each back-edge yields one loop, and downstream
/// passes can merge them if needed. (LICM only cares about the body
/// set, so identical-header loops are still safe.)
pub fn find_natural_loops(func: &Function) -> Vec<NaturalLoop> {
    let doms = compute_dominators(func);
    let preds = predecessors(func);

    // Collect back edges: (latch → header) where header dominates latch.
    let mut back_edges: Vec<(BlockId, BlockId)> = Vec::new();
    for block in &func.blocks {
        if let Some(term) = &block.terminator {
            for tgt in terminator_targets(term) {
                if let Some(d) = doms.get(&block.id) {
                    if d.contains(&tgt) {
                        back_edges.push((block.id, tgt));
                    }
                }
            }
        }
    }

    // Group back edges by header so we get one NaturalLoop per header.
    let mut by_header: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for (latch, hdr) in back_edges {
        by_header.entry(hdr).or_default().push(latch);
    }

    // Audit N-4: iterate the grouped back-edges in **stable** header
    // order. `HashMap` iteration order is nondeterministic across
    // runs, which would make downstream passes that hoist loop-by-loop
    // (like LICM) produce different IR on repeated compiles of the
    // same source — painful for `--emit-ir` bisection and for
    // differential debugging. Sort by `header.0` (and latches within
    // each loop for the same reason).
    let mut headers: Vec<BlockId> = by_header.keys().copied().collect();
    headers.sort_by_key(|h| h.0);

    let mut loops = Vec::new();
    for header in headers {
        let mut latches = by_header.remove(&header).unwrap();
        latches.sort_by_key(|l| l.0);
        // Body = {header} ∪ all nodes that can reach any latch without
        // going through the header.
        let mut body: HashSet<BlockId> = HashSet::new();
        body.insert(header);
        for &latch in &latches {
            body.insert(latch);
        }
        // Audit M-7: filter out latches that *are* the header
        // (self-loops). If we walk preds(header) the BFS will
        // enumerate the preheader as a "body" node and absorb
        // everything reachable from the entry, which both inflates
        // the body and makes `find_preheader` see no out-of-loop
        // predecessor for the header.
        let mut stack: Vec<BlockId> = latches.iter()
            .filter(|&&l| l != header)
            .copied()
            .collect();
        while let Some(b) = stack.pop() {
            if let Some(plist) = preds.get(&b) {
                for &p in plist {
                    if p == header { continue; }
                    if body.insert(p) {
                        stack.push(p);
                    }
                }
            }
        }
        loops.push(NaturalLoop { header, body, latches });
    }
    loops
}

/// Remove blocks unreachable from the function entry. Returns true if
/// any blocks were dropped.
///
/// Audit M4-2 / m4-3: uses `try_block` instead of `block` so a stale
/// terminator target (pointing at a block that was already pruned,
/// e.g., mid-pass state) degrades gracefully to "skip that edge"
/// instead of panicking. On valid IR this behaves identically to the
/// old version because the verifier rejects dangling targets.
pub fn prune_unreachable(func: &mut Function) -> bool {
    let mut reachable: HashSet<BlockId> = HashSet::new();
    let mut queue: VecDeque<BlockId> = VecDeque::new();
    queue.push_back(func.entry);
    reachable.insert(func.entry);
    while let Some(bid) = queue.pop_front() {
        let Some(block) = func.try_block(bid) else { continue };
        if let Some(term) = &block.terminator {
            for tgt in terminator_targets(term) {
                if reachable.insert(tgt) {
                    queue.push_back(tgt);
                }
            }
        }
    }
    let before = func.blocks.len();
    func.blocks.retain(|b| reachable.contains(&b.id));
    func.blocks.len() != before
}
