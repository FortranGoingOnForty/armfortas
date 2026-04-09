//! Mem2Reg — promote scalar stack slots into pure SSA values.
//!
//! Walks every function looking for `Alloca` slots whose address
//! never escapes (the only uses are `Load(alloca)` and
//! `Store(_, alloca)`), then rewrites the function so that each
//! such slot becomes a flow of SSA values: every `Load` is replaced
//! with a direct reference to the "current" value of the slot, every
//! `Store` updates that current value, and at control-flow merges
//! we insert **block parameters** (our IR's phi-node equivalent)
//! plus matching branch args in every predecessor.
//!
//! This is the classical Cytron-Ferrante-Rosen-Wegman-Zadeck
//! algorithm adapted for the block-parameter IR:
//!
//!   1. **Find promotable allocas**. An alloca is promotable iff
//!      every use of its result ValueId is either a `Load(alloca)`
//!      or a `Store(v, alloca)`. Any other use (as a GEP base, a
//!      Call arg, an `ExtractField` aggregate, etc.) means the
//!      address could escape and we can't safely promote.
//!
//!   2. **Compute phi placements** via iterated dominance frontiers
//!      over the set of blocks that store to each promotable alloca.
//!      For each such frontier block we reserve a fresh `BlockParam`
//!      that will carry the alloca's value at that join point.
//!
//!   3. **Insert block params** with `Undef` initial values. Record
//!      the `(alloca → block → new_param_id)` mapping so we can
//!      append matching branch args during renaming.
//!
//!   4. **Renaming walk**. Do a DFS of the dominator tree maintaining
//!      a per-alloca stack of "current value". Entering a block
//!      pushes any new params belonging to that block; every `Store`
//!      pushes its value; every `Load` is replaced by a reference
//!      to the stack-top and its defining instruction is marked
//!      dead. Leaving a block pops everything we pushed. At each
//!      terminator, append the current value of each phi alloca to
//!      the corresponding successor's branch arg list.
//!
//!   5. **Delete** the now-dead `Alloca`, `Load`, and `Store`
//!      instructions. DCE would clean them up eventually but doing
//!      it here makes the IR tidy immediately and gives downstream
//!      passes (LICM in particular) a cleaner view.
//!
//! ## Why this matters
//!
//! Before mem2reg, every Fortran local lives in an alloca slot and
//! is accessed via `Load`/`Store`. LICM conservatively refuses to
//! hoist `Load` (no alias analysis), so loop invariants never move.
//! After mem2reg, the same locals become pure SSA values: invariant
//! computations are visible to LICM, CSE dedupes across former
//! store/load pairs, and const_fold propagates constants through
//! local variables at compile time. Most of the optimizer's hoped-for
//! wins are unlocked by this pass.
//!
//! ## Scope
//!
//! This pass handles **scalar allocas only**. Aggregate allocas
//! (fixed-size arrays, structs) require SROA — scalar replacement
//! of aggregates — which decomposes them into individual scalars
//! first. SROA is a separate pass that will land alongside the rest
//! of Sprint 29's memory optimizations.

use super::pass::Pass;
use super::util::{
    compute_dominance_frontiers,
    compute_immediate_dominators,
    dominator_tree_children,
    prune_unreachable,
    substitute_uses,
};
use crate::ir::inst::*;
use crate::ir::types::IrType;
use std::collections::{HashMap, HashSet, VecDeque};

/// The mem2reg scalar promotion pass.
pub struct Mem2Reg;

impl Pass for Mem2Reg {
    fn name(&self) -> &'static str { "mem2reg" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if promote_function(func) {
                changed = true;
            }
        }
        changed
    }
}

/// An alloca eligible for promotion. `alloca_id` is the `ValueId`
/// produced by the original `Alloca` instruction; `pointee_ty` is
/// the type of the slot's contents.
struct Promotable {
    alloca_id: ValueId,
    pointee_ty: IrType,
}

fn promote_function(func: &mut Function) -> bool {
    // ---- Phase 0: drop unreachable blocks ------------------------
    //
    // Audit M1: every later phase walks `func.blocks` and assumes
    // that any block it visits is reachable from entry. Without
    // this prune:
    //
    //  * Phase 2 (`store_blocks`) collects stores from unreachable
    //    blocks, which causes the iterated-DF closure to insert
    //    phi params even though no rename walk will visit those
    //    blocks (the rename walk uses the dom-tree, which itself
    //    excludes unreachable nodes).
    //
    //  * Phase 4 (rename walk) never visits unreachable blocks, so
    //    their `Load`/`Store` instructions never make it into
    //    `dead_loads` / `dead_stores`. Phase 5 then deletes the
    //    underlying alloca but leaves the dangling load/store
    //    referencing it — invalid IR that the verifier rejects.
    //
    // Pruning first makes "every block we look at is reachable" a
    // structural invariant of the rest of the pass.
    let pruned = prune_unreachable(func);

    // ---- Phase 1: find promotable allocas -------------------------
    let promotable = find_promotable_allocas(func);
    if promotable.is_empty() { return pruned; }

    // Map from alloca ValueId → index into `promotable`. We use the
    // index as a compact key throughout the rest of the pass.
    let alloca_index: HashMap<ValueId, usize> = promotable.iter()
        .enumerate()
        .map(|(i, p)| (p.alloca_id, i))
        .collect();

    // ---- Phase 2: compute phi insertion blocks --------------------
    // For each alloca, collect the set of blocks that store to it.
    let mut store_blocks: Vec<HashSet<BlockId>> = vec![HashSet::new(); promotable.len()];
    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::Store(_, addr) = &inst.kind {
                if let Some(&idx) = alloca_index.get(addr) {
                    store_blocks[idx].insert(block.id);
                }
            }
        }
    }

    // Iterated dominance frontier: for each alloca, closure of DF
    // over its store-block set.
    let df = compute_dominance_frontiers(func);
    let mut phi_blocks: Vec<HashSet<BlockId>> = vec![HashSet::new(); promotable.len()];
    for (idx, stores) in store_blocks.iter().enumerate() {
        let mut worklist: VecDeque<BlockId> = stores.iter().copied().collect();
        let mut in_phi: HashSet<BlockId> = HashSet::new();
        while let Some(b) = worklist.pop_front() {
            if let Some(frontier) = df.get(&b) {
                for &y in frontier {
                    if in_phi.insert(y) {
                        // Newly added to phi set. Re-inserting the
                        // frontier node into the worklist handles
                        // the iterated DF closure — if y itself is
                        // a store-block-in-effect now, its own
                        // frontier contributes too.
                        worklist.push_back(y);
                    }
                }
            }
        }
        phi_blocks[idx] = in_phi;
    }

    // ---- Phase 3: insert block params + Undef sentinels ------------
    //
    // For each alloca, emit ONE `Undef` instruction at the top of
    // the entry block. This is the initial "current value" of the
    // alloca — any `Load` that executes before the first `Store`
    // reads undef, which is semantically correct (Fortran doesn't
    // define the value of an uninitialized local).
    //
    // For each (alloca, phi-block) pair, emit a new `BlockParam`.
    // The alloca's "current value" on entry to that block is the
    // new param. During the rename walk we'll append matching
    // branch args from predecessors.

    // (alloca_idx, block_id) → new param ValueId.
    let mut phi_params: HashMap<(usize, BlockId), ValueId> = HashMap::new();

    // Per alloca: its initial Undef ValueId at function entry.
    let mut undef_values: Vec<ValueId> = Vec::with_capacity(promotable.len());

    // Grab a dummy span we can reuse for inserted insts.
    let span = func.block(func.entry).insts.first()
        .map(|i| i.span)
        .or_else(|| {
            func.block(func.entry).terminator.as_ref().map(|_t| {
                // If the block is terminator-only, fall back to a
                // zero span — good enough for synthesized insts.
                crate::lexer::Span {
                    start: crate::lexer::Position { line: 0, col: 0 },
                    end:   crate::lexer::Position { line: 0, col: 0 },
                    file_id: 0,
                }
            })
        })
        .unwrap_or(crate::lexer::Span {
            start: crate::lexer::Position { line: 0, col: 0 },
            end:   crate::lexer::Position { line: 0, col: 0 },
            file_id: 0,
        });

    // Insert each Undef sentinel at the very front of the entry
    // block so it dominates every use. Audit Med-3: the previous
    // version computed `pos = undef_values.len() - 1` which only
    // worked because nothing else lived at the front of entry —
    // a future pass adding prologue insts would break it. The
    // unconditional `insert(0, ...)` is robust regardless of what
    // sits below it. We insert in REVERSE order so promotable[0]
    // ends up at insts[0] after all inserts complete.
    let entry = func.entry;
    let mut new_undefs: Vec<(ValueId, IrType)> = Vec::with_capacity(promotable.len());
    for p in &promotable {
        let id = func.next_value_id();
        undef_values.push(id);
        new_undefs.push((id, p.pointee_ty.clone()));
    }
    for (id, ty) in new_undefs.iter().rev() {
        func.block_mut(entry).insts.insert(0, Inst {
            id: *id,
            kind: InstKind::Undef(ty.clone()),
            ty: ty.clone(),
            span,
        });
    }

    // Now insert block params. Order within a block matters: we
    // append in `promotable.len()` order so the rename walk can
    // correspond each new param to its alloca index.
    //
    // `block_phi_order[block]` is the list of alloca indices that
    // have a phi param at `block`, in the order the params were
    // appended. This also defines the order in which we'll append
    // branch args at predecessors.
    let mut block_phi_order: HashMap<BlockId, Vec<usize>> = HashMap::new();

    for idx in 0..promotable.len() {
        // Process phi blocks in deterministic order (by BlockId.0).
        let mut blocks: Vec<BlockId> = phi_blocks[idx].iter().copied().collect();
        blocks.sort_by_key(|b| b.0);
        for bid in blocks {
            if bid == func.entry {
                // CFRWZ never places entry in any block's dominance
                // frontier — entry has no predecessors, so it can't
                // be a join point. If we ever see entry in
                // `phi_blocks` it means the DF computation is buggy.
                // Skip in release for safety; assert loudly in debug
                // so the bug surfaces during development. The
                // initial Undef value already handles the
                // "before-any-store" case at entry.
                debug_assert!(
                    false,
                    "mem2reg: entry block appeared in phi placement set for alloca {} \
                     — DF should never include entry (entry has no preds)",
                    idx,
                );
                continue;
            }
            let pid = func.next_value_id();
            func.block_mut(bid).params.push(BlockParam {
                id: pid,
                ty: promotable[idx].pointee_ty.clone(),
            });
            phi_params.insert((idx, bid), pid);
            block_phi_order.entry(bid).or_default().push(idx);
        }
    }

    // ---- Phase 4: renaming walk (DFS over dominator tree) ---------
    //
    // Per-alloca current-value stack. The stack top is the SSA value
    // that an instruction executing at the current point of the walk
    // should see when it loads from this alloca.
    //
    // We also accumulate `load_renames`: (old load ValueId → new
    // SSA ValueId) which we apply to the whole function via a
    // single `substitute_uses_batch` call at the end. The loads and
    // stores themselves are marked for deletion.
    //
    // Visiting a block in the dom tree:
    //   1. For each new phi param at this block: push its ValueId.
    //   2. Walk instructions: handle Store/Load for promotable
    //      allocas. Track how many times we pushed onto each stack
    //      so we can pop on the way out.
    //   3. For each successor: append the current stack top of each
    //      phi-alloca at that successor to the terminator's matching
    //      arg list.
    //   4. Recurse into each dominator-tree child.
    //   5. Pop everything we pushed in this block.

    let idoms = compute_immediate_dominators(func);
    let children = dominator_tree_children(&idoms);

    // Current value stack per alloca. Initialized with the entry
    // Undef for each alloca so that any Load before any Store on
    // this dom-tree path sees undef.
    let mut stacks: Vec<Vec<ValueId>> = undef_values.iter()
        .map(|&u| vec![u])
        .collect();

    // (old_load_id, new_value_id) rewrites to apply at the end.
    let mut load_renames: HashMap<ValueId, ValueId> = HashMap::new();
    // Instructions (block_id, inst index at the time of marking)
    // that should be deleted. We collect ValueIds and do a pass at
    // the end instead of tracking indices, which are invalidated
    // by other in-place mutations during the walk.
    let mut dead_loads: HashSet<ValueId> = HashSet::new();
    let mut dead_stores: HashSet<ValueId> = HashSet::new();

    // Helper closure to process a block. Returns the number of
    // stack-pushes we need to pop per alloca.
    #[allow(clippy::too_many_arguments)]
    fn rename_block(
        func: &mut Function,
        block_id: BlockId,
        promotable: &[Promotable],
        alloca_index: &HashMap<ValueId, usize>,
        phi_params: &HashMap<(usize, BlockId), ValueId>,
        block_phi_order: &HashMap<BlockId, Vec<usize>>,
        children: &HashMap<BlockId, Vec<BlockId>>,
        stacks: &mut [Vec<ValueId>],
        load_renames: &mut HashMap<ValueId, ValueId>,
        dead_loads: &mut HashSet<ValueId>,
        dead_stores: &mut HashSet<ValueId>,
    ) {
        // 1. Push phi params into their alloca stacks.
        let mut pushes_per_alloca: Vec<usize> = vec![0; promotable.len()];
        if let Some(order) = block_phi_order.get(&block_id) {
            for &idx in order {
                if let Some(&pid) = phi_params.get(&(idx, block_id)) {
                    stacks[idx].push(pid);
                    pushes_per_alloca[idx] += 1;
                }
            }
        }

        // 2. Walk instructions. We do this as an index loop because
        // we may need to rewrite the current inst in place (for
        // Load → delete, Store → delete).
        let block_len = func.block(block_id).insts.len();
        for i in 0..block_len {
            let inst = &func.block(block_id).insts[i];
            let inst_id = inst.id;
            match &inst.kind {
                InstKind::Load(addr) => {
                    let addr = *addr;
                    if let Some(&idx) = alloca_index.get(&addr) {
                        // This is a load from a promotable alloca.
                        // Rewrite uses of `inst_id` to the current
                        // stack top.
                        let cur = resolve_promoted_value(
                            *stacks[idx].last().expect("mem2reg: stack empty at load"),
                            load_renames,
                        );
                        load_renames.insert(inst_id, cur);
                        dead_loads.insert(inst_id);
                    }
                }
                InstKind::Store(val, addr) => {
                    let val = resolve_promoted_value(*val, load_renames);
                    let addr = *addr;
                    if let Some(&idx) = alloca_index.get(&addr) {
                        stacks[idx].push(val);
                        pushes_per_alloca[idx] += 1;
                        dead_stores.insert(inst_id);
                    }
                }
                _ => {}
            }
        }

        // 3. Append branch args for each successor's phi params.
        //
        // We need to know the successors and mutate their terminator
        // arg lists. `func.block_mut` gives us a single-borrow view,
        // so we collect successor IDs first.
        //
        // Audit M2: dedupe successors before iterating. A
        // `CondBranch { true: B, false: B, .. }` (or a `Switch`
        // whose default and a case both branch to the same block)
        // would otherwise visit `B` twice. Each visit appends a
        // copy of the new args to BOTH the true_args and
        // false_args slots inside `append_branch_args_for`,
        // doubling the arg lists. The verifier then rejects the
        // resulting function for branch-arg / param count mismatch.
        let successors: Vec<BlockId> = {
            let block = func.block(block_id);
            let raw: Vec<BlockId> = match &block.terminator {
                Some(Terminator::Return(_)) | Some(Terminator::Unreachable) | None => vec![],
                Some(Terminator::Branch(d, _)) => vec![*d],
                Some(Terminator::CondBranch { true_dest, false_dest, .. }) => {
                    vec![*true_dest, *false_dest]
                }
                Some(Terminator::Switch { cases, default, .. }) => {
                    let mut v: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
                    v.push(*default);
                    v
                }
            };
            let mut seen: HashSet<BlockId> = HashSet::new();
            raw.into_iter().filter(|b| seen.insert(*b)).collect()
        };
        for succ in successors {
            if let Some(order) = block_phi_order.get(&succ) {
                // For each alloca with a phi at succ, append the
                // current stack top as a branch arg. The order
                // matches the order the params were pushed onto
                // `succ.params` during phase 3.
                let new_args: Vec<ValueId> = order.iter()
                    .map(|&idx| resolve_promoted_value(
                        *stacks[idx].last().expect("mem2reg: stack empty at branch"),
                        load_renames,
                    ))
                    .collect();
                // Locate the slots in the terminator's arg list.
                let block_mut = func.block_mut(block_id);
                if let Some(term) = &mut block_mut.terminator {
                    append_branch_args_for(term, succ, &new_args);
                }
            }
        }

        // 4. Recurse into children.
        let kids: Vec<BlockId> = children.get(&block_id).cloned().unwrap_or_default();
        for kid in kids {
            rename_block(
                func, kid, promotable, alloca_index, phi_params,
                block_phi_order, children, stacks,
                load_renames, dead_loads, dead_stores,
            );
        }

        // 5. Pop everything we pushed in this block.
        for (idx, count) in pushes_per_alloca.iter().enumerate() {
            for _ in 0..*count {
                stacks[idx].pop();
            }
        }
    }

    rename_block(
        func, func.entry, &promotable, &alloca_index, &phi_params,
        &block_phi_order, &children, &mut stacks,
        &mut load_renames, &mut dead_loads, &mut dead_stores,
    );

    // ---- Phase 5: apply load renames and delete dead insts --------
    for (old, new) in &load_renames {
        substitute_uses(func, *old, *new);
    }

    // Drop dead loads, stores, and the allocas themselves.
    let alloca_ids: HashSet<ValueId> = promotable.iter().map(|p| p.alloca_id).collect();
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if dead_loads.contains(&inst.id) { return false; }
            if dead_stores.contains(&inst.id) { return false; }
            if alloca_ids.contains(&inst.id) { return false; }
            true
        });
    }

    true
}

fn resolve_promoted_value(
    mut value: ValueId,
    load_renames: &HashMap<ValueId, ValueId>,
) -> ValueId {
    let mut seen = HashSet::new();
    while let Some(&next) = load_renames.get(&value) {
        if !seen.insert(value) || next == value {
            break;
        }
        value = next;
    }
    value
}

/// Find alloca instructions whose only uses are `Load(alloca)` and
/// `Store(v, alloca)` where the alloca is in the address position.
///
/// Any other use — GEP base, Call argument, ExtractField aggregate,
/// store's value slot, return value, branch arg, etc. — means the
/// address could be observed and we can't safely promote the slot.
fn find_promotable_allocas(func: &Function) -> Vec<Promotable> {
    // First pass: collect all alloca insts and their pointee type.
    let mut candidates: HashMap<ValueId, IrType> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::Alloca(inner) = &inst.kind {
                candidates.insert(inst.id, inner.clone());
            }
        }
    }
    if candidates.is_empty() { return Vec::new(); }

    // Second pass: walk every use and disqualify any alloca whose
    // ValueId appears in a non-load/non-store-addr position.
    for block in &func.blocks {
        for inst in &block.insts {
            match &inst.kind {
                InstKind::Alloca(_) => {}
                InstKind::Load(addr) => {
                    // addr is fine — loading from an alloca is a
                    // promotable use. Nothing to do.
                    let _ = addr;
                }
                InstKind::Store(val, addr) => {
                    // `val` is an ordinary use — if `val` is an
                    // alloca, that means we're storing the slot's
                    // address somewhere. That's an escape.
                    if candidates.contains_key(val) {
                        candidates.remove(val);
                    }
                    // `addr` being an alloca is the promotable
                    // case. Nothing to do.
                    let _ = addr;
                }
                _ => {
                    // Any other instruction: every operand that is
                    // an alloca disqualifies that alloca.
                    for op in crate::ir::walk::inst_uses(&inst.kind) {
                        if candidates.contains_key(&op) {
                            candidates.remove(&op);
                        }
                    }
                }
            }
        }
        // Terminators can carry ValueIds too (Return value, branch
        // args, cond). Any of those being an alloca means the
        // slot's address flows somewhere observable.
        if let Some(term) = &block.terminator {
            for v in crate::ir::walk::terminator_uses(term) {
                if candidates.contains_key(&v) {
                    candidates.remove(&v);
                }
            }
        }
    }

    // Build the `Promotable` list in a deterministic order — by
    // the order the allocas appear in the function. This matters
    // for test repeatability and for the "undef values land at
    // entry.insts[0..promotable.len()]" layout invariant used
    // during phase 3.
    let mut out = Vec::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::Alloca(_) = &inst.kind {
                if let Some(ty) = candidates.remove(&inst.id) {
                    out.push(Promotable { alloca_id: inst.id, pointee_ty: ty });
                }
            }
        }
    }
    out
}

/// Append `new_args` to the argument slot(s) in `term` that branch
/// to `target`.
///
/// A `CondBranch` whose `true_dest == false_dest == target` has TWO
/// arg slots that branch to `target` (one per arm), and both arms
/// must receive the same append: the new value of the alloca at the
/// source block doesn't depend on which arm is taken at runtime, so
/// both arg lists end up identical for the new param. The caller
/// must ensure this function is invoked **once per unique successor**
/// (not once per edge), otherwise the arg lists double up. The
/// rename walk in `rename_block` enforces this via successor dedup.
fn append_branch_args_for(term: &mut Terminator, target: BlockId, new_args: &[ValueId]) {
    match term {
        Terminator::Branch(d, args) if *d == target => {
            args.extend_from_slice(new_args);
        }
        Terminator::CondBranch { true_dest, true_args, false_dest, false_args, .. } => {
            if *true_dest == target {
                true_args.extend_from_slice(new_args);
            }
            if *false_dest == target {
                false_args.extend_from_slice(new_args);
            }
        }
        // Switch targets cannot have block params per our IR
        // convention (the verifier enforces this). Nothing to do.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IntWidth;
    use crate::ir::verify::verify_module;
    use crate::lexer::{Span, Position};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span { start: p, end: p, file_id: 0 }
    }

    fn push_inst(f: &mut Function, block: BlockId, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        f.block_mut(block).insts.push(Inst { id, kind, ty, span: dummy_span() });
        id
    }

    // =============================================================
    // Straight-line promotion: single store + single load should
    // dissolve into a direct value flow.
    // =============================================================
    #[test]
    fn straight_line_single_store_load() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;

        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c7 = push_inst(&mut f, entry,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, entry,
            InstKind::Store(c7, slot),
            IrType::Void,
        );
        let loaded = push_inst(&mut f, entry,
            InstKind::Load(slot),
            IrType::Int(IntWidth::I32),
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(loaded)));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        // After: alloca/store/load all gone. The return should
        // reference c7 directly (via substitution).
        let block = &m.functions[0].blocks[0];
        assert!(!block.insts.iter().any(|i| matches!(i.kind, InstKind::Alloca(_))),
            "alloca should be gone");
        assert!(!block.insts.iter().any(|i| matches!(i.kind, InstKind::Load(_))),
            "load should be gone");
        assert!(!block.insts.iter().any(|i| matches!(i.kind, InstKind::Store(..))),
            "store should be gone");
        match block.terminator.as_ref().unwrap() {
            Terminator::Return(Some(v)) => assert_eq!(*v, c7,
                "return should reference the stored const directly"),
            _ => panic!(),
        }
    }

    // =============================================================
    // Diamond merge: if cond { x = 1; } else { x = 2; } load x.
    // The merge block should grow a block param for x.
    // =============================================================
    #[test]
    fn diamond_merge_inserts_block_param() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;

        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let cond = push_inst(&mut f, entry,
            InstKind::ConstBool(true),
            IrType::Bool,
        );
        let then_b = f.create_block("then");
        let else_b = f.create_block("else");
        let merge = f.create_block("merge");
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond, true_dest: then_b, true_args: vec![],
            false_dest: else_b, false_args: vec![],
        });

        let c1 = push_inst(&mut f, then_b,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, then_b,
            InstKind::Store(c1, slot), IrType::Void,
        );
        f.block_mut(then_b).terminator = Some(Terminator::Branch(merge, vec![]));

        let c2 = push_inst(&mut f, else_b,
            InstKind::ConstInt(2, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, else_b,
            InstKind::Store(c2, slot), IrType::Void,
        );
        f.block_mut(else_b).terminator = Some(Terminator::Branch(merge, vec![]));

        let loaded = push_inst(&mut f, merge,
            InstKind::Load(slot),
            IrType::Int(IntWidth::I32),
        );
        f.block_mut(merge).terminator = Some(Terminator::Return(Some(loaded)));

        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        // The merge block should now have exactly one block param
        // (for the promoted alloca), and the return should reference
        // that param.
        let f = &m.functions[0];
        let merge_block = f.block(merge);
        assert_eq!(merge_block.params.len(), 1, "merge should have 1 block param");
        let param_id = merge_block.params[0].id;
        match merge_block.terminator.as_ref().unwrap() {
            Terminator::Return(Some(v)) => assert_eq!(*v, param_id,
                "return should reference the merge block param"),
            _ => panic!(),
        }

        // Each predecessor's branch to merge must now carry one arg
        // equal to the value it would have stored.
        let then_term = f.block(then_b).terminator.as_ref().unwrap();
        match then_term {
            Terminator::Branch(_, args) => {
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], c1);
            }
            _ => panic!(),
        }
        let else_term = f.block(else_b).terminator.as_ref().unwrap();
        match else_term {
            Terminator::Branch(_, args) => {
                assert_eq!(args.len(), 1);
                assert_eq!(args[0], c2);
            }
            _ => panic!(),
        }
    }

    // =============================================================
    // Non-promotable: the alloca's address escapes via a Call.
    // mem2reg must leave it alone.
    // =============================================================
    #[test]
    fn escaping_alloca_not_promoted() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;
        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        // Call that takes the slot's address — escape!
        push_inst(&mut f, entry,
            InstKind::Call(FuncRef::External("takes_ptr".into()), vec![slot]),
            IrType::Void,
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Mem2Reg.run(&mut m), "escaping alloca should not be promoted");
        // The alloca is still there.
        assert!(m.functions[0].blocks[0].insts.iter()
            .any(|i| matches!(i.kind, InstKind::Alloca(_))));
    }

    // =============================================================
    // Mix of promotable + non-promotable: only the good one goes.
    // =============================================================
    #[test]
    fn mix_promotable_and_escaping() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;
        // Promotable: only used by store + load.
        let good = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        // Non-promotable: escapes via call.
        let bad = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c42 = push_inst(&mut f, entry,
            InstKind::ConstInt(42, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, entry,
            InstKind::Store(c42, good), IrType::Void,
        );
        push_inst(&mut f, entry,
            InstKind::Call(FuncRef::External("takes_ptr".into()), vec![bad]),
            IrType::Void,
        );
        let loaded = push_inst(&mut f, entry,
            InstKind::Load(good), IrType::Int(IntWidth::I32),
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(loaded)));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "IR invalid: {:?}", errs);

        let block = &m.functions[0].blocks[0];
        // `good` is gone; `bad` remains.
        let alloca_count = block.insts.iter()
            .filter(|i| matches!(i.kind, InstKind::Alloca(_)))
            .count();
        assert_eq!(alloca_count, 1, "bad alloca should survive");
        // Return should reference c42 directly.
        match block.terminator.as_ref().unwrap() {
            Terminator::Return(Some(v)) => assert_eq!(*v, c42),
            _ => panic!(),
        }
    }

    // =============================================================
    // Loop: counter alloca `i` initialized to 0, incremented each
    // iteration. Mem2reg should promote `i` and insert a block param
    // on the loop header.
    // =============================================================
    #[test]
    fn loop_counter_promoted_with_header_param() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;
        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c0 = push_inst(&mut f, entry,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, entry,
            InstKind::Store(c0, slot), IrType::Void,
        );

        let header = f.create_block("header");
        let body = f.create_block("body");
        let exit = f.create_block("exit");
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![]));

        // header: load i, cmp i < 10, cond br body/exit
        let cur = push_inst(&mut f, header,
            InstKind::Load(slot), IrType::Int(IntWidth::I32),
        );
        let c10 = push_inst(&mut f, header,
            InstKind::ConstInt(10, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let cmp = push_inst(&mut f, header,
            InstKind::ICmp(CmpOp::Lt, cur, c10),
            IrType::Bool,
        );
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp, true_dest: body, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });

        // body: i = i + 1; br header
        let cur2 = push_inst(&mut f, body,
            InstKind::Load(slot), IrType::Int(IntWidth::I32),
        );
        let c1 = push_inst(&mut f, body,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let next = push_inst(&mut f, body,
            InstKind::IAdd(cur2, c1),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, body,
            InstKind::Store(next, slot), IrType::Void,
        );
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![]));

        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "IR invalid: {:?}", errs);

        let f = &m.functions[0];
        let header_block = f.block(header);
        // Header should have exactly one block param (the promoted counter).
        assert_eq!(header_block.params.len(), 1,
            "header should have 1 block param for the promoted counter");
        // No loads or stores anywhere.
        for b in &f.blocks {
            for i in &b.insts {
                assert!(!matches!(i.kind, InstKind::Load(_)),
                    "no loads should survive mem2reg");
                assert!(!matches!(i.kind, InstKind::Store(..)),
                    "no stores should survive mem2reg");
                assert!(!matches!(i.kind, InstKind::Alloca(_)),
                    "no allocas should survive mem2reg");
            }
        }
    }

    // =============================================================
    // Load before store: reads undef. Verify the rename walks
    // correctly and the IR stays valid (the undef sentinel takes
    // the load's place).
    // =============================================================
    #[test]
    fn load_before_any_store_reads_undef() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;
        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let loaded = push_inst(&mut f, entry,
            InstKind::Load(slot),
            IrType::Int(IntWidth::I32),
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(loaded)));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        // Return should reference the synthetic Undef.
        let f = &m.functions[0];
        let undef_id = f.blocks[0].insts.iter()
            .find(|i| matches!(i.kind, InstKind::Undef(_)))
            .map(|i| i.id)
            .expect("no Undef inserted");
        match f.blocks[0].terminator.as_ref().unwrap() {
            Terminator::Return(Some(v)) => assert_eq!(*v, undef_id),
            _ => panic!(),
        }
    }

    // =============================================================
    // Audit M1 — unreachable block storing to a promoted alloca
    // must not corrupt the IR. mem2reg's Phase 0 pre-prune should
    // drop the unreachable block before it influences the
    // dominance frontier or leaves dangling load/store insts.
    // =============================================================
    #[test]
    fn unreachable_block_with_store_does_not_corrupt_ir() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;

        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c1 = push_inst(&mut f, entry,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, entry, InstKind::Store(c1, slot), IrType::Void);
        let loaded = push_inst(&mut f, entry,
            InstKind::Load(slot),
            IrType::Int(IntWidth::I32),
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(loaded)));

        // Build an UNREACHABLE block that also stores to `slot`.
        // Nothing branches to it from entry, so prune_unreachable
        // should remove it before the rename walk.
        let dead = f.create_block("dead");
        let c99 = push_inst(&mut f, dead,
            InstKind::ConstInt(99, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, dead, InstKind::Store(c99, slot), IrType::Void);
        f.block_mut(dead).terminator = Some(Terminator::Return(None));

        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(),
            "post-mem2reg IR invalid (unreachable store regression): {:?}", errs);

        let f = &m.functions[0];
        // The dead block must be gone (pruned by Phase 0).
        assert!(!f.blocks.iter().any(|b| b.id == dead),
            "unreachable block should be pruned by mem2reg Phase 0");
        // The promoted alloca and its load/store should be gone.
        let entry_block = f.block(f.entry);
        assert!(!entry_block.insts.iter().any(|i| matches!(i.kind, InstKind::Alloca(_))));
        assert!(!entry_block.insts.iter().any(|i| matches!(i.kind, InstKind::Load(_))));
        assert!(!entry_block.insts.iter().any(|i| matches!(i.kind, InstKind::Store(..))));
        // Return must reference c1 (the live store value).
        match entry_block.terminator.as_ref().unwrap() {
            Terminator::Return(Some(v)) => assert_eq!(*v, c1,
                "return should reach c1 directly, not the dead block's c99"),
            _ => panic!(),
        }
    }

    // =============================================================
    // Audit M2 — a CondBranch with the same target on both arms
    // must not double-append branch args during the rename walk.
    // =============================================================
    #[test]
    fn same_target_cond_branch_does_not_double_append() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;

        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c1 = push_inst(&mut f, entry,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, entry, InstKind::Store(c1, slot), IrType::Void);

        let then_b = f.create_block("then");
        let merge = f.create_block("merge");

        // Conditional branch where both arms target `then_b`. The
        // pre-fix mem2reg would visit `then_b` twice via the
        // successor list and double-append the new branch args.
        let cond = push_inst(&mut f, entry,
            InstKind::ConstBool(true), IrType::Bool,
        );
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: then_b,
            true_args: vec![],
            false_dest: then_b,
            false_args: vec![],
        });

        // `then_b` stores a different value, then branches to merge.
        let c2 = push_inst(&mut f, then_b,
            InstKind::ConstInt(2, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, then_b, InstKind::Store(c2, slot), IrType::Void);
        f.block_mut(then_b).terminator = Some(Terminator::Branch(merge, vec![]));

        // `merge` loads from slot and returns it.
        let loaded = push_inst(&mut f, merge,
            InstKind::Load(slot), IrType::Int(IntWidth::I32),
        );
        f.block_mut(merge).terminator = Some(Terminator::Return(Some(loaded)));

        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(),
            "post-mem2reg IR invalid (same-target CondBranch regression): {:?}", errs);

        // Verify the entry's CondBranch has at most ONE arg per arm
        // (the new phi-arg added for the promoted slot). Pre-fix it
        // would have TWO per arm.
        let f = &m.functions[0];
        let entry_block = f.block(f.entry);
        match entry_block.terminator.as_ref().unwrap() {
            Terminator::CondBranch { true_args, false_args, .. } => {
                assert!(true_args.len() <= 1,
                    "true_args double-appended: {:?}", true_args);
                assert!(false_args.len() <= 1,
                    "false_args double-appended: {:?}", false_args);
            }
            // mem2reg may have collapsed the cond_branch to a
            // direct branch if the cond was constant; that's
            // fine — verifier-clean is what we care about.
            _ => {}
        }
    }

    // =============================================================
    // Audit D2 — `compute_dominance_frontiers` must not populate
    // entries for unreachable predecessors. mem2reg shouldn't see
    // phantom DF entries from dead code.
    // =============================================================
    #[test]
    fn dominance_frontier_excludes_unreachable_preds() {
        use crate::ir::walk::compute_dominance_frontiers;
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let merge = f.create_block("merge");
        let dead = f.create_block("dead");

        // entry → merge (reachable)
        // dead → merge (unreachable, but emits an edge into merge)
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Branch(merge, vec![]));
        f.block_mut(dead).terminator = Some(Terminator::Branch(merge, vec![]));
        f.block_mut(merge).terminator = Some(Terminator::Return(None));

        let df = compute_dominance_frontiers(&f);

        // The dead block is unreachable; it should not appear in
        // the DF map at all.
        assert!(!df.contains_key(&dead),
            "DF map should not contain unreachable block, got {:?}", df);
        // The merge block has only ONE reachable predecessor
        // (entry), so it isn't a true join point and merge ∉ any
        // DF set.
        for (b, frontier) in &df {
            assert!(!frontier.contains(&merge),
                "merge should not be in DF[{:?}]: only one reachable pred", b);
        }
    }

    // =============================================================
    // Coverage: only-loaded alloca. An alloca that has loads but
    // no stores anywhere should still be promotable — every load
    // becomes Undef. Catches accidental "must have at least one
    // store" assumptions in the algorithm.
    // =============================================================
    #[test]
    fn only_loaded_alloca_promotes_to_undef() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;
        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        // Two loads with no intervening store.
        let l1 = push_inst(&mut f, entry,
            InstKind::Load(slot), IrType::Int(IntWidth::I32),
        );
        let _ = push_inst(&mut f, entry,
            InstKind::Load(slot), IrType::Int(IntWidth::I32),
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(l1)));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m), "no-store alloca should still be promotable");
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        let block = &m.functions[0].blocks[0];
        assert!(!block.insts.iter().any(|i| matches!(i.kind, InstKind::Alloca(_))),
            "alloca should be gone");
        assert!(!block.insts.iter().any(|i| matches!(i.kind, InstKind::Load(_))),
            "loads should be gone");
        // Both loads should be replaced by an Undef sentinel.
        assert!(block.insts.iter().any(|i| matches!(i.kind, InstKind::Undef(_))),
            "Undef sentinel should be inserted");
    }

    // =============================================================
    // Coverage: address-escape via store of the alloca pointer
    // into another memory location. The alloca whose address is
    // stored must be considered escaped (the writer could keep the
    // pointer for later) and must NOT be promoted.
    // =============================================================
    #[test]
    fn address_escape_via_store_blocks_promotion() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;

        // bag: a Ptr<Ptr<i32>> slot that we'll write the escape into.
        let bag = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        // escapee: the alloca whose address we leak.
        let escapee = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        // Store the escapee POINTER into bag — escapee escapes.
        push_inst(&mut f, entry,
            InstKind::Store(escapee, bag), IrType::Void,
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        // Either the pass returns false (nothing promoted) or it
        // promotes only `bag` and leaves `escapee` standing. What
        // it MUST NOT do is promote `escapee` away.
        let _ = Mem2Reg.run(&mut m);
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        let block = &m.functions[0].blocks[0];
        let surviving_allocas = block.insts.iter()
            .filter(|i| matches!(i.kind, InstKind::Alloca(_)))
            .count();
        assert!(
            surviving_allocas >= 1,
            "the address-escaped alloca must survive promotion, got {} allocas",
            surviving_allocas,
        );
    }

    // =============================================================
    // Coverage: idempotency. Running mem2reg twice on the same IR
    // must be a no-op the second time around.
    // =============================================================
    #[test]
    fn second_mem2reg_run_is_a_noop() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;

        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let cond = push_inst(&mut f, entry, InstKind::ConstBool(true), IrType::Bool);
        let then_b = f.create_block("then");
        let else_b = f.create_block("else");
        let merge = f.create_block("merge");
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond, true_dest: then_b, true_args: vec![],
            false_dest: else_b, false_args: vec![],
        });

        let c1 = push_inst(&mut f, then_b,
            InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, then_b, InstKind::Store(c1, slot), IrType::Void);
        f.block_mut(then_b).terminator = Some(Terminator::Branch(merge, vec![]));

        let c2 = push_inst(&mut f, else_b,
            InstKind::ConstInt(2, IntWidth::I32), IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, else_b, InstKind::Store(c2, slot), IrType::Void);
        f.block_mut(else_b).terminator = Some(Terminator::Branch(merge, vec![]));

        let loaded = push_inst(&mut f, merge,
            InstKind::Load(slot), IrType::Int(IntWidth::I32),
        );
        f.block_mut(merge).terminator = Some(Terminator::Return(Some(loaded)));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m), "first run should promote");
        // Second run must be a no-op: nothing left to promote.
        assert!(!Mem2Reg.run(&mut m), "second run on already-promoted IR should be a no-op");
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);
    }

    // =============================================================
    // Coverage: two promotable allocas joining at the same merge.
    // After promotion the merge block should have TWO block params,
    // and each predecessor should pass two args in the right order.
    // =============================================================
    #[test]
    fn two_allocas_same_merge_inserts_two_params() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = f.entry;

        let slot_a = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let slot_b = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let cond = push_inst(&mut f, entry, InstKind::ConstBool(true), IrType::Bool);
        let then_b = f.create_block("then");
        let else_b = f.create_block("else");
        let merge = f.create_block("merge");
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond, true_dest: then_b, true_args: vec![],
            false_dest: else_b, false_args: vec![],
        });

        // then: a=1, b=10
        let c1 = push_inst(&mut f, then_b,
            InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let c10 = push_inst(&mut f, then_b,
            InstKind::ConstInt(10, IntWidth::I32), IrType::Int(IntWidth::I32));
        push_inst(&mut f, then_b, InstKind::Store(c1, slot_a), IrType::Void);
        push_inst(&mut f, then_b, InstKind::Store(c10, slot_b), IrType::Void);
        f.block_mut(then_b).terminator = Some(Terminator::Branch(merge, vec![]));

        // else: a=2, b=20
        let c2 = push_inst(&mut f, else_b,
            InstKind::ConstInt(2, IntWidth::I32), IrType::Int(IntWidth::I32));
        let c20 = push_inst(&mut f, else_b,
            InstKind::ConstInt(20, IntWidth::I32), IrType::Int(IntWidth::I32));
        push_inst(&mut f, else_b, InstKind::Store(c2, slot_a), IrType::Void);
        push_inst(&mut f, else_b, InstKind::Store(c20, slot_b), IrType::Void);
        f.block_mut(else_b).terminator = Some(Terminator::Branch(merge, vec![]));

        // merge: result = a + b
        let la = push_inst(&mut f, merge,
            InstKind::Load(slot_a), IrType::Int(IntWidth::I32));
        let lb = push_inst(&mut f, merge,
            InstKind::Load(slot_b), IrType::Int(IntWidth::I32));
        let sum = push_inst(&mut f, merge,
            InstKind::IAdd(la, lb), IrType::Int(IntWidth::I32));
        f.block_mut(merge).terminator = Some(Terminator::Return(Some(sum)));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        let f = &m.functions[0];
        let merge_block = f.block(merge);
        assert_eq!(merge_block.params.len(), 2,
            "merge should have 2 block params, one per promoted alloca");
        // Each predecessor must now carry 2 branch args.
        for pred in [then_b, else_b] {
            let term = f.block(pred).terminator.as_ref().unwrap();
            match term {
                Terminator::Branch(_, args) => assert_eq!(args.len(), 2,
                    "predecessor {:?} should pass 2 args to merge", pred),
                _ => panic!("predecessor terminator should be Branch"),
            }
        }
    }

    // =============================================================
    // Regression: storing a freshly-loaded promoted value into a
    // second promotable slot inside a branch must not leave the
    // second slot's later loads pointing at the deleted load ID.
    // Audit 29.8 surfaced this from a SAVE-backed branchy scalar
    // shape lowered from real Fortran.
    // =============================================================
    #[test]
    fn branchy_store_of_promoted_load_keeps_ir_valid() {
        let mut m = Module::new("t".into());
        let mut f = Function::new(
            "f".into(),
            vec![Param {
                name: "flag".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
            }],
            IrType::Int(IntWidth::I32),
        );
        let entry = f.entry;

        let result_slot = push_inst(
            &mut f,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let save_slot = push_inst(
            &mut f,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let tmp_slot = push_inst(
            &mut f,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c7 = push_inst(
            &mut f,
            entry,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, entry, InstKind::Store(c7, save_slot), IrType::Void);
        let zero = push_inst(
            &mut f,
            entry,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let cond = push_inst(
            &mut f,
            entry,
            InstKind::ICmp(CmpOp::Gt, ValueId(0), zero),
            IrType::Bool,
        );
        let then_b = f.create_block("then");
        let else_b = f.create_block("else");
        let merge = f.create_block("merge");
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: then_b,
            true_args: vec![],
            false_dest: else_b,
            false_args: vec![],
        });

        let then_load = push_inst(
            &mut f,
            then_b,
            InstKind::Load(save_slot),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, then_b, InstKind::Store(then_load, tmp_slot), IrType::Void);
        let then_tmp = push_inst(
            &mut f,
            then_b,
            InstKind::Load(tmp_slot),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, then_b, InstKind::Store(then_tmp, result_slot), IrType::Void);
        f.block_mut(then_b).terminator = Some(Terminator::Branch(merge, vec![]));

        let else_load = push_inst(
            &mut f,
            else_b,
            InstKind::Load(save_slot),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, else_b, InstKind::Store(else_load, tmp_slot), IrType::Void);
        let else_tmp = push_inst(
            &mut f,
            else_b,
            InstKind::Load(tmp_slot),
            IrType::Int(IntWidth::I32),
        );
        let one = push_inst(
            &mut f,
            else_b,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let bumped = push_inst(
            &mut f,
            else_b,
            InstKind::IAdd(else_tmp, one),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut f, else_b, InstKind::Store(bumped, result_slot), IrType::Void);
        f.block_mut(else_b).terminator = Some(Terminator::Branch(merge, vec![]));

        let merged = push_inst(
            &mut f,
            merge,
            InstKind::Load(result_slot),
            IrType::Int(IntWidth::I32),
        );
        f.block_mut(merge).terminator = Some(Terminator::Return(Some(merged)));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m), "branchy chain should be promotable");
        let errs = verify_module(&m);
        assert!(
            errs.is_empty(),
            "post-mem2reg IR invalid for branchy load/store chain: {:?}",
            errs
        );

        let f = &m.functions[0];
        for block in &f.blocks {
            for inst in &block.insts {
                assert!(
                    !matches!(inst.kind, InstKind::Alloca(_) | InstKind::Load(_) | InstKind::Store(_, _)),
                    "promoted branchy scalar should leave no memory traffic, found {:?}",
                    inst.kind
                );
            }
        }
    }

    // =============================================================
    // Coverage: multi-store-per-iteration. A loop body that stores
    // to the same alloca twice in one iteration must end up with
    // the LAST store flowing back to the header.
    // =============================================================
    #[test]
    fn multi_store_per_iteration_uses_last_store() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;

        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c0 = push_inst(&mut f, entry,
            InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        push_inst(&mut f, entry, InstKind::Store(c0, slot), IrType::Void);

        let header = f.create_block("header");
        let body = f.create_block("body");
        let exit = f.create_block("exit");
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![]));

        // header: i = load slot; cmp i < 5
        let cur = push_inst(&mut f, header,
            InstKind::Load(slot), IrType::Int(IntWidth::I32));
        let c5 = push_inst(&mut f, header,
            InstKind::ConstInt(5, IntWidth::I32), IrType::Int(IntWidth::I32));
        let cmp = push_inst(&mut f, header,
            InstKind::ICmp(CmpOp::Lt, cur, c5), IrType::Bool);
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp, true_dest: body, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });

        // body: store cur+1 to slot, then store (cur+1)+10 to slot.
        // The SECOND store is the one that should flow to header.
        let c1 = push_inst(&mut f, body,
            InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let cur2 = push_inst(&mut f, body,
            InstKind::Load(slot), IrType::Int(IntWidth::I32));
        let plus1 = push_inst(&mut f, body,
            InstKind::IAdd(cur2, c1), IrType::Int(IntWidth::I32));
        push_inst(&mut f, body, InstKind::Store(plus1, slot), IrType::Void);
        let c10 = push_inst(&mut f, body,
            InstKind::ConstInt(10, IntWidth::I32), IrType::Int(IntWidth::I32));
        let plus10 = push_inst(&mut f, body,
            InstKind::IAdd(plus1, c10), IrType::Int(IntWidth::I32));
        push_inst(&mut f, body, InstKind::Store(plus10, slot), IrType::Void);
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![]));

        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        let f = &m.functions[0];
        // No loads/stores/allocas anywhere.
        for b in &f.blocks {
            for i in &b.insts {
                assert!(!matches!(i.kind, InstKind::Load(_) | InstKind::Store(..) | InstKind::Alloca(_)),
                    "should be promoted away: {:?}", i.kind);
            }
        }
        // body's branch back to header should pass `plus10` (the
        // last-store value), not `plus1`.
        let body_term = f.block(body).terminator.as_ref().unwrap();
        match body_term {
            Terminator::Branch(_, args) => {
                assert_eq!(args.len(), 1, "body should pass 1 arg to header");
                assert_eq!(args[0], plus10,
                    "header arg should be the LAST store value, not the first");
            }
            _ => panic!("body terminator should be Branch"),
        }
    }

    // =============================================================
    // Coverage: multi-latch loop. Two latch blocks both branch back
    // to the same header. Each latch must contribute its own arg
    // to the header's promoted block param.
    // =============================================================
    #[test]
    fn multi_latch_loop_each_latch_contributes_arg() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;

        let slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c0 = push_inst(&mut f, entry,
            InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        push_inst(&mut f, entry, InstKind::Store(c0, slot), IrType::Void);

        let header = f.create_block("header");
        let body = f.create_block("body");
        let latch_a = f.create_block("latch_a");
        let latch_b = f.create_block("latch_b");
        let exit = f.create_block("exit");
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![]));

        // header: i = load; cmp i < 100; cond br body / exit
        let cur = push_inst(&mut f, header,
            InstKind::Load(slot), IrType::Int(IntWidth::I32));
        let c100 = push_inst(&mut f, header,
            InstKind::ConstInt(100, IntWidth::I32), IrType::Int(IntWidth::I32));
        let cmp_top = push_inst(&mut f, header,
            InstKind::ICmp(CmpOp::Lt, cur, c100), IrType::Bool);
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp_top, true_dest: body, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });

        // body: branch to latch_a or latch_b based on cur.
        let c50 = push_inst(&mut f, body,
            InstKind::ConstInt(50, IntWidth::I32), IrType::Int(IntWidth::I32));
        let cmp_mid = push_inst(&mut f, body,
            InstKind::ICmp(CmpOp::Lt, cur, c50), IrType::Bool);
        f.block_mut(body).terminator = Some(Terminator::CondBranch {
            cond: cmp_mid, true_dest: latch_a, true_args: vec![],
            false_dest: latch_b, false_args: vec![],
        });

        // latch_a: store cur+1; jump header
        let c1a = push_inst(&mut f, latch_a,
            InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let curla = push_inst(&mut f, latch_a,
            InstKind::Load(slot), IrType::Int(IntWidth::I32));
        let nexta = push_inst(&mut f, latch_a,
            InstKind::IAdd(curla, c1a), IrType::Int(IntWidth::I32));
        push_inst(&mut f, latch_a, InstKind::Store(nexta, slot), IrType::Void);
        f.block_mut(latch_a).terminator = Some(Terminator::Branch(header, vec![]));

        // latch_b: store cur+2; jump header
        let c2b = push_inst(&mut f, latch_b,
            InstKind::ConstInt(2, IntWidth::I32), IrType::Int(IntWidth::I32));
        let curlb = push_inst(&mut f, latch_b,
            InstKind::Load(slot), IrType::Int(IntWidth::I32));
        let nextb = push_inst(&mut f, latch_b,
            InstKind::IAdd(curlb, c2b), IrType::Int(IntWidth::I32));
        push_inst(&mut f, latch_b, InstKind::Store(nextb, slot), IrType::Void);
        f.block_mut(latch_b).terminator = Some(Terminator::Branch(header, vec![]));

        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        let f = &m.functions[0];
        let header_block = f.block(header);
        assert_eq!(header_block.params.len(), 1,
            "header should have 1 block param for the counter");

        // Each latch's branch to header should pass exactly one arg
        // (its computed `next` value).
        let la_term = f.block(latch_a).terminator.as_ref().unwrap();
        match la_term {
            Terminator::Branch(_, args) => {
                assert_eq!(args.len(), 1, "latch_a passes 1 arg to header");
                assert_eq!(args[0], nexta);
            }
            _ => panic!("latch_a should branch to header"),
        }
        let lb_term = f.block(latch_b).terminator.as_ref().unwrap();
        match lb_term {
            Terminator::Branch(_, args) => {
                assert_eq!(args.len(), 1, "latch_b passes 1 arg to header");
                assert_eq!(args[0], nextb);
            }
            _ => panic!("latch_b should branch to header"),
        }
    }

    // =============================================================
    // Coverage: nested loops. An inner loop nested inside an outer
    // loop, both promoting their own counter. Both headers should
    // get a block param.
    // =============================================================
    #[test]
    fn nested_loops_promote_both_counters() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;

        let outer_slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let inner_slot = push_inst(&mut f, entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c0 = push_inst(&mut f, entry,
            InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        push_inst(&mut f, entry, InstKind::Store(c0, outer_slot), IrType::Void);

        let outer_header = f.create_block("outer_header");
        let inner_init  = f.create_block("inner_init");
        let inner_header = f.create_block("inner_header");
        let inner_body  = f.create_block("inner_body");
        let outer_latch = f.create_block("outer_latch");
        let exit = f.create_block("exit");

        f.block_mut(entry).terminator = Some(Terminator::Branch(outer_header, vec![]));

        // outer_header: i = load outer; cmp i<3; br inner_init / exit
        let i = push_inst(&mut f, outer_header,
            InstKind::Load(outer_slot), IrType::Int(IntWidth::I32));
        let c3 = push_inst(&mut f, outer_header,
            InstKind::ConstInt(3, IntWidth::I32), IrType::Int(IntWidth::I32));
        let cmpo = push_inst(&mut f, outer_header,
            InstKind::ICmp(CmpOp::Lt, i, c3), IrType::Bool);
        f.block_mut(outer_header).terminator = Some(Terminator::CondBranch {
            cond: cmpo, true_dest: inner_init, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });

        // inner_init: store 0 to inner; br inner_header
        let c0i = push_inst(&mut f, inner_init,
            InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        push_inst(&mut f, inner_init, InstKind::Store(c0i, inner_slot), IrType::Void);
        f.block_mut(inner_init).terminator = Some(Terminator::Branch(inner_header, vec![]));

        // inner_header: j = load inner; cmp j<5; br inner_body / outer_latch
        let j = push_inst(&mut f, inner_header,
            InstKind::Load(inner_slot), IrType::Int(IntWidth::I32));
        let c5 = push_inst(&mut f, inner_header,
            InstKind::ConstInt(5, IntWidth::I32), IrType::Int(IntWidth::I32));
        let cmpi = push_inst(&mut f, inner_header,
            InstKind::ICmp(CmpOp::Lt, j, c5), IrType::Bool);
        f.block_mut(inner_header).terminator = Some(Terminator::CondBranch {
            cond: cmpi, true_dest: inner_body, true_args: vec![],
            false_dest: outer_latch, false_args: vec![],
        });

        // inner_body: j = j + 1; store inner; br inner_header
        let c1i = push_inst(&mut f, inner_body,
            InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let jcur = push_inst(&mut f, inner_body,
            InstKind::Load(inner_slot), IrType::Int(IntWidth::I32));
        let jnext = push_inst(&mut f, inner_body,
            InstKind::IAdd(jcur, c1i), IrType::Int(IntWidth::I32));
        push_inst(&mut f, inner_body, InstKind::Store(jnext, inner_slot), IrType::Void);
        f.block_mut(inner_body).terminator = Some(Terminator::Branch(inner_header, vec![]));

        // outer_latch: i = i + 1; store outer; br outer_header
        let c1o = push_inst(&mut f, outer_latch,
            InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let icur = push_inst(&mut f, outer_latch,
            InstKind::Load(outer_slot), IrType::Int(IntWidth::I32));
        let inext = push_inst(&mut f, outer_latch,
            InstKind::IAdd(icur, c1o), IrType::Int(IntWidth::I32));
        push_inst(&mut f, outer_latch, InstKind::Store(inext, outer_slot), IrType::Void);
        f.block_mut(outer_latch).terminator = Some(Terminator::Branch(outer_header, vec![]));

        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Mem2Reg.run(&mut m));
        let errs = verify_module(&m);
        assert!(errs.is_empty(), "post-mem2reg IR invalid: {:?}", errs);

        let f = &m.functions[0];
        // Audit Min-2: pin the exact param counts. Hand-traced
        // CFRWZ for this CFG produces:
        //   * outer_slot stores at {entry, outer_latch}, with
        //     DF(outer_latch) = {outer_header}, so the IDF is
        //     {outer_header}. → outer_header gets 1 param.
        //   * inner_slot stores at {inner_init, inner_body}, with
        //     DF(inner_init) = DF(inner_body) = {outer_header,
        //     inner_header} after IDF closure (the back-edge from
        //     outer_latch and the back-edge from inner_body both
        //     promote outer_header / inner_header into the
        //     frontier). → outer_header AND inner_header each
        //     get 1 param for inner_slot.
        // Totals: outer_header has 2, inner_header has 1.
        assert_eq!(f.block(outer_header).params.len(), 2,
            "outer_header should have exactly 2 params (outer + inner counter via back-edge IDF), got {}",
            f.block(outer_header).params.len());
        assert_eq!(f.block(inner_header).params.len(), 1,
            "inner_header should have exactly 1 param for inner counter, got {}",
            f.block(inner_header).params.len());
        // No loads/stores/allocas anywhere — both slots fully promoted.
        for b in &f.blocks {
            for i in &b.insts {
                assert!(!matches!(i.kind, InstKind::Load(_) | InstKind::Store(..) | InstKind::Alloca(_)),
                    "{:?}: kind {:?} should be promoted away", b.id, i.kind);
            }
        }
    }
}
