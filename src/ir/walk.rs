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
        InstKind::Undef(..) | InstKind::Alloca(..) |
        InstKind::GlobalAddr(..) => vec![],

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
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) |
        InstKind::PtrToInt(v) | InstKind::IntToPtr(v, _) => vec![*v],

        InstKind::Load(a) => vec![*a],
        InstKind::Store(v, a) => vec![*v, *a],
        InstKind::GetElementPtr(base, idxs) => {
            let mut uses = vec![*base];
            uses.extend(idxs);
            uses
        }

        InstKind::Call(FuncRef::Indirect(target), args) => {
            let mut uses = vec![*target];
            uses.extend(args);
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
        InstKind::Undef(..) | InstKind::Alloca(..) |
        InstKind::GlobalAddr(..) => {}

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
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) |
        InstKind::PtrToInt(v) | InstKind::IntToPtr(v, _) => r(v),

        InstKind::Load(a) => r(a),
        InstKind::Store(v, a) => { r(v); r(a); }
        InstKind::GetElementPtr(base, idxs) => {
            r(base);
            for i in idxs { r(i); }
        }

        InstKind::Call(FuncRef::Indirect(target), args) => {
            r(target);
            for a in args { r(a); }
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

// =====================================================================
//                Dominator tree + dominance frontiers
// =====================================================================

/// Compute the immediate dominator of every reachable block.
///
/// The immediate dominator `idom(B)` is the unique closest dominator
/// of `B` other than `B` itself. The entry block has no idom; every
/// other reachable block does.
///
/// Returns a map keyed only by reachable non-entry blocks.
/// Unreachable blocks have an empty dominator set (per
/// [`compute_dominators`]) and therefore no idom.
///
/// **Performance**: this is the naïve O(N³) construction — for each
/// block we filter its dominator set, then for each candidate scan
/// every other candidate's dominator set looking for the unique
/// "smallest". `compute_dominators` itself is O(N²·E) iterative
/// data-flow on top, so the asymptotic ceiling for the dominator
/// pipeline is roughly O(N³ + N²·E). That is fine for the
/// kilo-block functions today's frontend produces, but if we ever
/// start lowering huge SSA graphs (autovec spillovers, inlining
/// across modules) the right replacement is Lengauer–Tarjan, which
/// is O((N+E)·α(N)) in practice. Track separately as an
/// optimization-tier replacement, not a correctness fix.
pub fn compute_immediate_dominators(func: &Function) -> HashMap<BlockId, BlockId> {
    let doms = compute_dominators(func);
    let mut idoms: HashMap<BlockId, BlockId> = HashMap::new();

    for block in &func.blocks {
        if block.id == func.entry { continue; }
        let Some(my_doms) = doms.get(&block.id) else { continue };
        if my_doms.is_empty() { continue; } // unreachable

        // The immediate dominator is the dominator (other than
        // self) that is dominated by every other dominator (other
        // than self). Equivalently: the dominator that has the
        // largest dominator set — all other dominators of `block`
        // also dominate the idom.
        let candidates: Vec<BlockId> = my_doms.iter()
            .copied()
            .filter(|&d| d != block.id)
            .collect();

        let idom = candidates.iter().copied().find(|&cand| {
            // cand is idom iff no other candidate strictly
            // dominates it (only cand itself and cand's own
            // dominators do).
            let cand_doms = doms.get(&cand).cloned().unwrap_or_default();
            candidates.iter().all(|&other| {
                other == cand || cand_doms.contains(&other)
            })
        });

        if let Some(idom) = idom {
            idoms.insert(block.id, idom);
        }
    }

    idoms
}

/// Compute the **children** of each block in the dominator tree —
/// the inverse of the idom map.
///
/// `children[B]` is the set of blocks whose immediate dominator is
/// `B`. A dominator-tree traversal visits `B` then recurses into
/// `children[B]` in some order.
pub fn dominator_tree_children(idoms: &HashMap<BlockId, BlockId>)
    -> HashMap<BlockId, Vec<BlockId>>
{
    let mut children: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for (&child, &parent) in idoms {
        children.entry(parent).or_default().push(child);
    }
    // Sort each child list by BlockId.0 for deterministic iteration.
    for list in children.values_mut() {
        list.sort_by_key(|b| b.0);
    }
    children
}

/// Compute the dominance frontier of every reachable block.
///
/// The dominance frontier `DF(B)` is the set of blocks `X` such that
/// `B` dominates a predecessor of `X` but does **not** strictly
/// dominate `X` itself. Dominance frontiers are where phi nodes
/// (block parameters in our IR) must be inserted when promoting an
/// alloca that is stored to in `B`.
///
/// Uses the Cytron-Ferrante-Rosen-Wegman-Zadeck formulation:
/// for every join point `X` (block with ≥ 2 predecessors), walk
/// each predecessor `P` upward in the dominator tree, adding `X`
/// to `DF(runner)` until `runner == idom(X)`.
pub fn compute_dominance_frontiers(func: &Function)
    -> HashMap<BlockId, HashSet<BlockId>>
{
    let idoms = compute_immediate_dominators(func);
    let preds = predecessors(func);

    // Audit D2: build the set of reachable blocks (= keys present
    // in `idoms`, plus the entry block itself). Unreachable blocks
    // have no idom, so the runner walk would `match idoms.get(&runner)`
    // → `None` → `break`, but only AFTER inserting `x` into
    // `df[runner]` — populating `df` with phantom entries for
    // unreachable predecessors that downstream consumers (mem2reg's
    // iterated-DF closure in particular) must then defensively
    // ignore. Filtering at the source keeps the DF map clean.
    let reachable: HashSet<BlockId> = idoms.keys().copied()
        .chain(std::iter::once(func.entry))
        .collect();

    let mut df: HashMap<BlockId, HashSet<BlockId>> = HashMap::new();
    // Initialize empty frontiers for every reachable block so
    // callers can index without checking for `Some`. Unreachable
    // blocks are excluded entirely from the map.
    for block in &func.blocks {
        if reachable.contains(&block.id) {
            df.insert(block.id, HashSet::new());
        }
    }

    for block in &func.blocks {
        let x = block.id;
        if !reachable.contains(&x) { continue; }
        let plist = preds.get(&x).cloned().unwrap_or_default();
        // Drop unreachable predecessors before counting toward
        // "join point" status. An unreachable predecessor adds no
        // runtime control flow into x.
        let plist: Vec<BlockId> = plist.into_iter()
            .filter(|p| reachable.contains(p))
            .collect();
        if plist.len() < 2 { continue; }

        // `x` is a join point. Walk each predecessor upward.
        let idom_x = idoms.get(&x).copied();
        for p in plist {
            let mut runner = p;
            // Stop when runner reaches idom(x) — at that point
            // `runner` strictly dominates `x`, so `x` is no longer
            // in `DF(runner)`.
            while Some(runner) != idom_x {
                df.entry(runner).or_default().insert(x);
                match idoms.get(&runner) {
                    Some(&parent) => runner = parent,
                    None => break, // runner is entry (no idom)
                }
            }
        }
    }

    df
}

/// Compute a **preorder** traversal of the dominator tree starting
/// at the entry block. This is the correct visit order for mem2reg's
/// renaming walk: a block's dominator's values are installed on the
/// current-value stack before we enter the block.
///
/// Returns a deterministic ordering (children are iterated in
/// ascending `BlockId.0` order courtesy of `dominator_tree_children`).
pub fn dominator_tree_preorder(func: &Function) -> Vec<BlockId> {
    let idoms = compute_immediate_dominators(func);
    let children = dominator_tree_children(&idoms);

    let mut order = Vec::new();
    let mut stack: Vec<BlockId> = vec![func.entry];
    while let Some(b) = stack.pop() {
        order.push(b);
        if let Some(kids) = children.get(&b) {
            // Push in reverse so that smaller BlockId.0 is visited first.
            for &k in kids.iter().rev() {
                stack.push(k);
            }
        }
    }
    order
}

#[cfg(test)]
mod walk_tests {
    use super::*;
    use super::super::types::IrType;
    use crate::lexer::{Span, Position};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span { start: p, end: p, file_id: 0 }
    }

    /// Build a diamond CFG:
    ///      entry
    ///      /   \
    ///     a     b
    ///      \   /
    ///      merge
    /// Used by dom-tree and dominance-frontier tests.
    fn diamond_function() -> (Function, BlockId, BlockId, BlockId) {
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let a = f.create_block("a");
        let b = f.create_block("b");
        let merge = f.create_block("merge");

        let entry = f.entry;
        let cond = f.next_value_id();
        f.block_mut(entry).insts.push(Inst {
            id: cond,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: a,
            true_args: vec![],
            false_dest: b,
            false_args: vec![],
        });
        f.block_mut(a).terminator = Some(Terminator::Branch(merge, vec![]));
        f.block_mut(b).terminator = Some(Terminator::Branch(merge, vec![]));
        f.block_mut(merge).terminator = Some(Terminator::Return(None));

        (f, a, b, merge)
    }

    #[test]
    fn immediate_dominators_diamond() {
        let (f, a, b, merge) = diamond_function();
        let idoms = compute_immediate_dominators(&f);
        // entry has no idom.
        assert!(!idoms.contains_key(&f.entry));
        // a, b, merge all have entry as their idom.
        assert_eq!(idoms[&a], f.entry);
        assert_eq!(idoms[&b], f.entry);
        assert_eq!(idoms[&merge], f.entry);
    }

    #[test]
    fn dominance_frontier_diamond() {
        let (f, a, b, merge) = diamond_function();
        let df = compute_dominance_frontiers(&f);
        // a's frontier is {merge} — a dominates itself (a pred of
        // merge) but doesn't dominate merge.
        assert_eq!(df[&a], HashSet::from([merge]));
        // b's frontier is also {merge}.
        assert_eq!(df[&b], HashSet::from([merge]));
        // merge's frontier is empty (it's a join point itself, but
        // has no successors that are join points of anything else
        // in this CFG).
        assert!(df[&merge].is_empty());
        // entry's frontier is empty.
        assert!(df[&f.entry].is_empty());
    }

    #[test]
    fn dominator_tree_preorder_visits_entry_first() {
        let (f, _a, _b, _merge) = diamond_function();
        let order = dominator_tree_preorder(&f);
        assert_eq!(order[0], f.entry);
        assert_eq!(order.len(), 4);
    }

    /// A simple loop:
    ///   entry → header → body → header
    ///                  → exit
    /// Body dominates nothing below header; header dominates body,
    /// exit, and the back-edge doesn't change dominance.
    #[test]
    fn dominance_frontier_loop() {
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let header = f.create_block("header");
        let body = f.create_block("body");
        let exit = f.create_block("exit");

        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![]));

        let cond = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: cond,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        let df = compute_dominance_frontiers(&f);
        // `body`'s frontier is {header} — body is a pred of header
        // via the back edge, but doesn't dominate header.
        assert_eq!(df[&body], HashSet::from([header]));
        // `header`'s frontier is {header} too — header is a join
        // point (entry + body both branch into it), and though
        // header dominates itself, it does NOT *strictly* dominate
        // itself, so header is in its own frontier.
        assert_eq!(df[&header], HashSet::from([header]));
    }

    /// `find_promotable-style` usage: verify the idom map is what
    /// mem2reg would read. Nested if: both branches store to %a;
    /// both merge blocks need a block param. `iterated_dominance_frontier`
    /// is effectively DF applied transitively over the store set.
    #[test]
    fn iterated_dominance_frontier_via_repeated_df() {
        // Build:
        //    entry
        //     / \
        //    a   b
        //    |  / \
        //    | c   d
        //    |  \ /
        //    |   m1
        //    \  /
        //     m2
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let a = f.create_block("a");
        let b = f.create_block("b");
        let c = f.create_block("c");
        let d = f.create_block("d");
        let m1 = f.create_block("m1");
        let m2 = f.create_block("m2");

        let entry = f.entry;
        let c0 = f.next_value_id();
        f.block_mut(entry).insts.push(Inst {
            id: c0, kind: InstKind::ConstBool(true),
            ty: IrType::Bool, span: dummy_span(),
        });
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond: c0, true_dest: a, true_args: vec![],
            false_dest: b, false_args: vec![],
        });
        let c1 = f.next_value_id();
        f.block_mut(b).insts.push(Inst {
            id: c1, kind: InstKind::ConstBool(true),
            ty: IrType::Bool, span: dummy_span(),
        });
        f.block_mut(b).terminator = Some(Terminator::CondBranch {
            cond: c1, true_dest: c, true_args: vec![],
            false_dest: d, false_args: vec![],
        });
        f.block_mut(c).terminator = Some(Terminator::Branch(m1, vec![]));
        f.block_mut(d).terminator = Some(Terminator::Branch(m1, vec![]));
        f.block_mut(m1).terminator = Some(Terminator::Branch(m2, vec![]));
        f.block_mut(a).terminator = Some(Terminator::Branch(m2, vec![]));
        f.block_mut(m2).terminator = Some(Terminator::Return(None));

        let df = compute_dominance_frontiers(&f);
        // c and d both flow into m1; DF(c) = DF(d) = {m1}
        assert_eq!(df[&c], HashSet::from([m1]));
        assert_eq!(df[&d], HashSet::from([m1]));
        // a and m1 both flow into m2; DF(a) = DF(m1) = {m2}
        assert_eq!(df[&a], HashSet::from([m2]));
        assert_eq!(df[&m1], HashSet::from([m2]));
        // b dominates c, d, m1 but not m2 — b is in df of m2 because
        // it dominates a predecessor of m2 (m1) but not m2 itself.
        assert_eq!(df[&b], HashSet::from([m2]));
        // The dominator tree looks like:
        //   entry → {a, b, m2}
        //   b     → {c, d, m1}
        let idoms = compute_immediate_dominators(&f);
        assert_eq!(idoms[&a], entry);
        assert_eq!(idoms[&b], entry);
        assert_eq!(idoms[&m2], entry);
        assert_eq!(idoms[&c], b);
        assert_eq!(idoms[&d], b);
        assert_eq!(idoms[&m1], b);
    }

    // ----- Edge case: single-block function -----
    //
    // A function whose entry block is also its only block must
    // produce an empty idom map (entry has no idom) and a single
    // empty dominance frontier entry.
    #[test]
    fn single_block_function_has_no_idom() {
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        let idoms = compute_immediate_dominators(&f);
        assert!(idoms.is_empty(), "single-block function should have no idoms");

        let df = compute_dominance_frontiers(&f);
        // Entry is reachable, so it has an entry in the DF map, but
        // its frontier is empty (no successors, no join points).
        assert_eq!(df.len(), 1);
        assert!(df[&f.entry].is_empty());

        let order = dominator_tree_preorder(&f);
        assert_eq!(order, vec![f.entry]);
    }

    // ----- Edge case: self-loop on entry -----
    //
    // The entry block has itself as a successor. Entry still has no
    // idom (it's the root), and entry is its own join point (1 pred,
    // itself), so it should NOT appear in any DF set.
    #[test]
    fn self_loop_on_entry() {
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        // entry → entry (always loops, no exit). Verifier doesn't run
        // here so an unreachable terminator is fine for the dom math.
        let cond = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: cond,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        let exit = f.create_block("exit");
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: f.entry, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        let idoms = compute_immediate_dominators(&f);
        assert!(!idoms.contains_key(&f.entry), "entry has no idom even with a self-edge");
        assert_eq!(idoms[&exit], f.entry);

        let df = compute_dominance_frontiers(&f);
        // The self-edge gives entry one in-edge (from itself); plus
        // the implicit "function entry edge" doesn't count, so entry
        // has 1 reachable pred. Not a join point — entry ∉ any DF.
        for (b, frontier) in &df {
            assert!(!frontier.contains(&f.entry),
                "entry should not appear in DF[{:?}]", b);
        }
    }

    // ----- Edge case: self-loop on a non-entry block -----
    //
    // entry → b → b   (b has both an entry edge and a self-edge,
    //                  so 2 reachable preds → join point.)
    // CFRWZ says b ∈ DF(b) here: b dominates itself (a pred of
    // itself) but does not strictly dominate itself.
    #[test]
    fn self_loop_on_non_entry_block_is_in_own_df() {
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let b = f.create_block("b");
        let exit = f.create_block("exit");

        f.block_mut(f.entry).terminator = Some(Terminator::Branch(b, vec![]));

        let cond = f.next_value_id();
        f.block_mut(b).insts.push(Inst {
            id: cond,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(b).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: b, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        let df = compute_dominance_frontiers(&f);
        assert_eq!(df[&b], HashSet::from([b]),
            "b's self-edge puts b in its own dominance frontier");
        let idoms = compute_immediate_dominators(&f);
        assert_eq!(idoms[&b], f.entry);
        assert_eq!(idoms[&exit], b);
    }

    // ----- Edge case: irreducible CFG -----
    //
    //   entry → a → b → a   (cross-edge into the loop)
    //   entry → b
    //
    // Both `a` and `b` have 2 preds, neither dominates the other,
    // and the loop has no single header. The dom math should still
    // produce a sensible answer: entry idom both, and each is in
    // the other's dominance frontier.
    #[test]
    fn irreducible_cfg_has_consistent_doms() {
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let a = f.create_block("a");
        let b = f.create_block("b");
        let exit = f.create_block("exit");

        // entry → cond ? a : b
        let c0 = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: c0, kind: InstKind::ConstBool(true),
            ty: IrType::Bool, span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: c0,
            true_dest: a, true_args: vec![],
            false_dest: b, false_args: vec![],
        });

        // a → b
        f.block_mut(a).terminator = Some(Terminator::Branch(b, vec![]));

        // b → cond ? a : exit (so b → a is a back edge into a, but
        //                      a is not the only header — entry → b
        //                      bypasses it).
        let c1 = f.next_value_id();
        f.block_mut(b).insts.push(Inst {
            id: c1, kind: InstKind::ConstBool(true),
            ty: IrType::Bool, span: dummy_span(),
        });
        f.block_mut(b).terminator = Some(Terminator::CondBranch {
            cond: c1,
            true_dest: a, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        let idoms = compute_immediate_dominators(&f);
        // Neither a nor b dominates the other; entry idoms both.
        assert_eq!(idoms[&a], f.entry);
        assert_eq!(idoms[&b], f.entry);
        assert_eq!(idoms[&exit], b);

        let df = compute_dominance_frontiers(&f);
        // a dominates a (a pred of b) → b ∈ DF(a) because a doesn't
        // strictly dominate b.
        assert!(df[&a].contains(&b), "b should be in DF(a)");
        // b dominates b (a pred of a) → a ∈ DF(b).
        assert!(df[&b].contains(&a), "a should be in DF(b)");

        // dominator_tree_preorder must terminate and visit every
        // reachable block once (no infinite loop on the irreducible
        // back edge).
        let order = dominator_tree_preorder(&f);
        assert_eq!(order.len(), 4); // entry, a, b, exit
        assert_eq!(order[0], f.entry);
    }
}
