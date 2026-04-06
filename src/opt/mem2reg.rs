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
    // ---- Phase 1: find promotable allocas -------------------------
    let promotable = find_promotable_allocas(func);
    if promotable.is_empty() { return false; }

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

    for p in &promotable {
        let id = func.next_value_id();
        undef_values.push(id);
        // Insert Undef at the *start* of the entry block so it
        // dominates every use. We push these in order so that the
        // undef for promotable[i] ends up at entry.insts[i].
        let entry = func.entry;
        let pos = {
            // Skip over existing Undef insts we just inserted so
            // the new ones line up. In practice this is `i`.
            undef_values.len() - 1
        };
        func.block_mut(entry).insts.insert(pos, Inst {
            id,
            kind: InstKind::Undef(p.pointee_ty.clone()),
            ty: p.pointee_ty.clone(),
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
                // The entry block shouldn't receive phi params —
                // if an alloca's frontier ever includes entry, the
                // initial Undef value already handles the "before
                // any store" case. Skip to stay consistent with
                // the verifier's "entry block has no params" rule.
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
                        let cur = *stacks[idx].last()
                            .expect("mem2reg: stack empty at load");
                        load_renames.insert(inst_id, cur);
                        dead_loads.insert(inst_id);
                    }
                }
                InstKind::Store(val, addr) => {
                    let val = *val;
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
        let successors: Vec<BlockId> = {
            let block = func.block(block_id);
            match &block.terminator {
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
            }
        };
        for succ in successors {
            if let Some(order) = block_phi_order.get(&succ) {
                // For each alloca with a phi at succ, append the
                // current stack top as a branch arg. The order
                // matches the order the params were pushed onto
                // `succ.params` during phase 3.
                let new_args: Vec<ValueId> = order.iter()
                    .map(|&idx| *stacks[idx].last()
                        .expect("mem2reg: stack empty at branch"))
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
/// to `target`. A terminator may branch to the same target twice
/// (e.g., `CondBranch { true: B, false: B }`), in which case both
/// slots receive the same append.
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
}
