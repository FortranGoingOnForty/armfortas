//! Dead-code elimination pass.
//!
//! Removes instructions whose result has no observable effect:
//!
//!  * The result `ValueId` is unused by any other instruction or
//!    terminator in the function.
//!  * The instruction has no side effects of its own (a `Store`,
//!    impure `Call`, hidden-result `Call`, or `RuntimeCall` is always
//!    live, even if its return is unused, because it can write memory
//!    or perform I/O).
//!
//! The pass uses mark-and-sweep:
//!
//! 1. Walk every block, every instruction, every terminator and mark
//!    each `ValueId` they consume as "live".
//! 2. Walk every instruction in every block; instructions whose result
//!    is unused **and** which are pure are dropped.
//!
//! Constant-folding plus constant-propagation can leave behind a lot
//! of pure dead computation (e.g., the `Const*` values that defined a
//! folded conditional). DCE cleans those up. We also remove blocks
//! that became unreachable for any reason (defensive — `ConstProp`
//! already prunes after itself, but other future passes may not).
//!
//! ### Side-effect classification
//!
//! In Fortran, the following are observable:
//!  * `Store` — writes user memory
//!  * `Call` / `RuntimeCall` — print, allocate, free, etc.
//!  * Scalar FP operations when the surrounding call graph accesses the
//!    floating-point environment — their unused value can still raise an
//!    IEEE status flag observed by the program.
//!  * `Return` / branches / `Unreachable` — terminators are *always*
//!    live; we never remove them.
//!
//! `Load` is conservatively pure here. That is technically incorrect
//! when the same address is later stored to (the load could be
//! observing volatile memory), but for SSA-form Fortran loads of
//! locals it's a safe approximation. A future alias-analysis pass can
//! refine this.

use super::pass::Pass;
use super::util::{inst_uses, prune_unreachable, terminator_uses};
use crate::ir::inst::*;
use crate::ir::types::IrType;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug)]
struct InternalCallDceInfo {
    discardable_when_unused: bool,
}

/// Return every address proven to remain within this invocation's stack frame.
///
/// A local alloca and GEPs derived directly from one can be written without
/// making a call externally observable. Pointer loads, block parameters,
/// integer round-trips, and other derived addresses stay conservative: a store
/// through any of them prevents call removal.
fn local_stack_addresses(func: &Function) -> HashSet<ValueId> {
    let mut local = HashSet::new();
    let mut derived_by_base: HashMap<ValueId, Vec<ValueId>> = HashMap::new();
    let mut worklist = VecDeque::new();

    for inst in func.blocks.iter().flat_map(|block| block.insts.iter()) {
        match &inst.kind {
            InstKind::Alloca(_) => {
                local.insert(inst.id);
                worklist.push_back(inst.id);
            }
            InstKind::GetElementPtr(base, _) => {
                derived_by_base.entry(*base).or_default().push(inst.id);
            }
            _ => {}
        }
    }

    while let Some(base) = worklist.pop_front() {
        if let Some(derived) = derived_by_base.get(&base) {
            for &address in derived {
                if local.insert(address) {
                    worklist.push_back(address);
                }
            }
        }
    }

    local
}

/// Collect internal callees when a function has no direct observable effect.
///
/// This is deliberately independent of `Function::is_pure`: that field records
/// the source-language attribute, while this scan is the optimizer proof needed
/// before deleting an actual call. Returning `None` means the body contains an
/// effect DCE cannot prove local.
fn effect_free_body_dependencies(func: &Function, preserve_fp_effects: bool) -> Option<Vec<u32>> {
    let local_addresses = local_stack_addresses(func);
    let mut internal_callees = Vec::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if preserve_fp_effects && super::fpenv::is_scalar_fpenv_sensitive(func, inst) {
                return None;
            }
            match &inst.kind {
                InstKind::Store(_, addr) | InstKind::VStore(_, addr)
                    if !local_addresses.contains(addr) =>
                {
                    return None;
                }
                InstKind::RuntimeCall(..) => return None,
                InstKind::Call(FuncRef::Internal(idx), _) => internal_callees.push(*idx),
                InstKind::Call(..) => return None,
                _ => {}
            }
        }
        if matches!(block.terminator.as_ref(), Some(Terminator::Unreachable)) {
            return None;
        }
    }

    Some(internal_callees)
}

/// Derive which internal calls can be discarded when their results are dead.
///
/// Candidates need both a non-void Fortran `PURE` annotation and an
/// independently inspected body with no direct observable effects. Internal
/// call dependencies are then solved as a greatest fixed point: starting with
/// all direct candidates preserves effect-free recursive cycles, while any
/// edge to an impure, effectful, void-returning, or missing callee poisons every
/// transitive caller.
fn derive_internal_call_dce_info_for_context(
    module: &Module,
    preserve_fp_effects: &[bool],
) -> Vec<InternalCallDceInfo> {
    debug_assert_eq!(module.functions.len(), preserve_fp_effects.len());
    let mut dependencies = Vec::with_capacity(module.functions.len());
    let mut info = Vec::with_capacity(module.functions.len());

    for (func_idx, func) in module.functions.iter().enumerate() {
        let body_dependencies = if !matches!(func.return_type, IrType::Void) && func.is_pure {
            effect_free_body_dependencies(func, preserve_fp_effects[func_idx])
        } else {
            None
        };
        let discardable_when_unused = body_dependencies.is_some();
        dependencies.push(body_dependencies.unwrap_or_default());
        info.push(InternalCallDceInfo {
            discardable_when_unused,
        });
    }

    let mut callers_by_callee = vec![Vec::new(); module.functions.len()];
    for (caller_idx, callees) in dependencies.iter().enumerate() {
        if !info[caller_idx].discardable_when_unused {
            continue;
        }
        for &callee in callees {
            let Some(callers) = callers_by_callee.get_mut(callee as usize) else {
                info[caller_idx].discardable_when_unused = false;
                break;
            };
            callers.push(caller_idx);
        }
    }

    let mut worklist = info
        .iter()
        .enumerate()
        .filter_map(|(idx, summary)| (!summary.discardable_when_unused).then_some(idx))
        .collect::<VecDeque<_>>();
    while let Some(effectful_idx) = worklist.pop_front() {
        for &caller_idx in &callers_by_callee[effectful_idx] {
            if info[caller_idx].discardable_when_unused {
                info[caller_idx].discardable_when_unused = false;
                worklist.push_back(caller_idx);
            }
        }
    }

    info
}

fn derive_internal_call_dce_info(module: &Module) -> Vec<InternalCallDceInfo> {
    let fpenv_effects = super::fpenv::analyze_fpenv_effects(module);
    derive_internal_call_dce_info_for_context(module, &fpenv_effects.may_run_in_dynamic_fpenv)
}

/// True if the instruction has any side effect that prevents removal,
/// regardless of whether its result is used.
fn has_side_effect(
    inst: &Inst,
    internal_calls: &[InternalCallDceInfo],
    preserved_fp_effects: &HashSet<ValueId>,
) -> bool {
    if preserved_fp_effects.contains(&inst.id) {
        return true;
    }
    matches!(
        &inst.kind,
        InstKind::Store(..)
            // VStore writes 128 bits to memory just like Store; it
            // must not be DCE'd. NeonVectorize emits VStore as the
            // sink of every vectorized loop body — without this,
            // DCE strips the whole body once it's been vectorized
            // (the VAdd/VLoad chain has no other uses) and the
            // function silently becomes a no-op.
            | InstKind::VStore(..)
            | InstKind::RuntimeCall(..)
            // Alloca produces a stack slot whose identity matters even
            // if no one reads from it (the address may escape via a
            // future store/call). Treat as side-effecting for safety.
            | InstKind::Alloca(..)
    ) || match &inst.kind {
        InstKind::Call(FuncRef::Internal(idx), _) => match internal_calls.get(*idx as usize) {
            Some(info) => !info.discardable_when_unused,
            None => true,
        },
        InstKind::Call(..) => true,
        _ => false,
    }
}

/// Collect every `ValueId` referenced as an operand of any instruction
/// or terminator in the function. The result is the "live use" set.
fn collect_live_uses(func: &Function) -> HashSet<ValueId> {
    let mut live = HashSet::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for v in inst_uses(&inst.kind) {
                live.insert(v);
            }
        }
        if let Some(term) = &block.terminator {
            for v in terminator_uses(term) {
                live.insert(v);
            }
        }
    }
    live
}

/// Run DCE over a single function. Returns true if anything changed.
///
/// Two interleaved fixpoints:
///
/// 1. **Inner**: instruction-level mark-and-sweep. We rebuild the
///    live set every round and drop any pure instruction whose
///    result has no uses. Removing one instruction can make another
///    dead (its operands lose a use), so we iterate.
///
/// 2. **Outer**: block-parameter cleanup. After the instructions
///    settle, we look for block parameters whose `ValueId` no
///    longer appears in the live-use set, drop them, and rewrite
///    every predecessor's branch arg list to remove the
///    corresponding slot. Removing an arg can free its defining
///    instruction, so we re-run the inner loop afterwards. Audit
///    finding Med-5 / C-1.
fn dce_function(
    func: &mut Function,
    internal_calls: &[InternalCallDceInfo],
    preserve_fp_effects: bool,
) -> bool {
    let preserved_fp_effects: HashSet<ValueId> = if preserve_fp_effects {
        func.blocks
            .iter()
            .flat_map(|block| block.insts.iter())
            .filter(|inst| super::fpenv::is_scalar_fpenv_sensitive(func, inst))
            .map(|inst| inst.id)
            .collect()
    } else {
        HashSet::new()
    };
    let mut any_change = false;
    let mut outer_changed = true;
    while outer_changed {
        outer_changed = false;

        // Inner: instruction-level mark-and-sweep.
        let mut changed = true;
        while changed {
            changed = false;
            let live = collect_live_uses(func);
            for block in &mut func.blocks {
                let before = block.insts.len();
                block.insts.retain(|inst| {
                    if has_side_effect(inst, internal_calls, &preserved_fp_effects) {
                        return true;
                    }
                    if live.contains(&inst.id) {
                        return true;
                    }
                    false
                });
                if block.insts.len() != before {
                    changed = true;
                    any_change = true;
                }
            }
        }

        // Outer: block-parameter cleanup.
        if remove_dead_block_params(func) {
            any_change = true;
            outer_changed = true;
        }
    }
    if prune_unreachable(func) {
        any_change = true;
    }
    any_change
}

/// Drop block parameters whose `ValueId` is unused, and rewrite every
/// predecessor's branch arg list to delete the matching slot. The
/// entry block is exempt (entry blocks aren't allowed to have
/// parameters anyway).
///
/// Returns `true` if at least one parameter was removed.
///
/// Audit B-5: the previous implementation walked `func.blocks` once
/// per `(target, idx)` pair via linear `find` — O(N²·K) for N blocks
/// and K dead params. The new version builds a `BlockId → vec_index`
/// map upfront so the per-pair work is O(N).
fn remove_dead_block_params(func: &mut Function) -> bool {
    let live = collect_live_uses(func);

    let mut by_block: HashMap<BlockId, Vec<usize>> = HashMap::new();
    for block in func.blocks.iter() {
        if block.id == func.entry {
            continue;
        }
        for (idx, p) in block.params.iter().enumerate() {
            if !live.contains(&p.id) {
                by_block.entry(block.id).or_default().push(idx);
            }
        }
    }
    if by_block.is_empty() {
        return false;
    }

    // Map BlockId → vec index. LICM/DCE never structurally reorder
    // `func.blocks`; only the inst vectors change. So this stays
    // valid for the lifetime of this call.
    let block_index: HashMap<BlockId, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();

    for (target_block, mut idxs) in by_block {
        idxs.sort_by(|a, b| b.cmp(a)); // descending — high indices first so removals don't shift later ones
        let target_idx = block_index[&target_block];
        for idx in idxs {
            // Remove the parameter at `idx` from `target_block`.
            let blk = &mut func.blocks[target_idx];
            if idx < blk.params.len() {
                blk.params.remove(idx);
            }
            // Drop the corresponding arg slot from every predecessor's
            // terminator that branches into `target_block`. We still
            // walk every block here because we don't keep an explicit
            // pred map (build one if profiling shows this dominates).
            for src in func.blocks.iter_mut() {
                if let Some(term) = &mut src.terminator {
                    drop_branch_arg(term, target_block, idx);
                }
            }
        }
    }
    true
}

/// In-place: if `term` branches to `target`, remove the arg at `idx`
/// from the corresponding arg vector.
///
/// Audit B-10: the previous version silently no-op'd when the index
/// was out of range. A mismatch between target params and predecessor
/// args is a real IR validity violation, so we now `debug_assert!` on
/// it instead. The release build still degrades gracefully (we don't
/// want to crash production compiles on a stale arg list — the
/// verifier will reject the resulting IR anyway), but a debug build
/// will surface the bug at the source.
fn drop_branch_arg(term: &mut Terminator, target: BlockId, idx: usize) {
    match term {
        Terminator::Branch(d, args) if *d == target => {
            debug_assert!(
                idx < args.len(),
                "drop_branch_arg: branch arg list out of sync with target params"
            );
            if idx < args.len() {
                args.remove(idx);
            }
        }
        Terminator::CondBranch {
            true_dest,
            true_args,
            false_dest,
            false_args,
            ..
        } => {
            if *true_dest == target {
                debug_assert!(
                    idx < true_args.len(),
                    "drop_branch_arg: cond_branch true_args out of sync"
                );
                if idx < true_args.len() {
                    true_args.remove(idx);
                }
            }
            if *false_dest == target {
                debug_assert!(
                    idx < false_args.len(),
                    "drop_branch_arg: cond_branch false_args out of sync"
                );
                if idx < false_args.len() {
                    false_args.remove(idx);
                }
            }
        }
        Terminator::Switch { cases, default, .. } => {
            // Switch targets cannot have block params per the
            // verifier (`check_branch_args` rejects them). Audit
            // M-6: if we ever see a Switch that branches into a
            // block we're stripping params from, that's a real IR
            // validity bug. Assert instead of silently skipping.
            let touches_target = cases.iter().any(|(_, b)| *b == target) || *default == target;
            debug_assert!(
                !touches_target,
                "drop_branch_arg: Switch terminator branches to a block with params \
                 — verifier should reject this construction"
            );
        }
        _ => {}
    }
}

pub struct Dce;

impl Pass for Dce {
    fn name(&self) -> &'static str {
        "dce"
    }
    fn run(&self, module: &mut Module) -> bool {
        let fpenv_effects = super::fpenv::analyze_fpenv_effects(module);
        // Unknown externals and the IEEE flag/status entry points are
        // conservatively classified as environment barriers. Their downward
        // closure identifies every internal function whose dead FP operations
        // may be observed by a caller's status query.
        let preserve_fp_effects = &fpenv_effects.may_run_in_dynamic_fpenv;
        let internal_calls = derive_internal_call_dce_info_for_context(module, preserve_fp_effects);
        let mut changed = false;
        for (func_idx, func) in module.functions.iter_mut().enumerate() {
            if dce_function(func, &internal_calls, preserve_fp_effects[func_idx]) {
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{FloatWidth, IntWidth, IrType};
    use crate::lexer::{Position, Span};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span {
            start: p,
            end: p,
            file_id: 0,
        }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id,
            kind,
            ty,
            span: dummy_span(),
        });
        id
    }

    fn internal_call_count(func: &Function, target: u32) -> usize {
        func.blocks
            .iter()
            .flat_map(|block| block.insts.iter())
            .filter(|inst| {
                matches!(
                    &inst.kind,
                    InstKind::Call(FuncRef::Internal(idx), _) if *idx == target
                )
            })
            .count()
    }

    #[test]
    fn removes_unused_const() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        push(
            &mut f,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dce.run(&mut m));
        assert!(m.functions[0].blocks[0].insts.is_empty());
    }

    #[test]
    fn keeps_const_used_by_return() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let v = push(
            &mut f,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(v)));
        m.add_function(f);

        assert!(!Dce.run(&mut m));
        assert_eq!(m.functions[0].blocks[0].insts.len(), 1);
    }

    #[test]
    fn keeps_store_even_if_address_unused() {
        // Alloca + Store should both stay even though no one loads.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let addr = push(
            &mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let v = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v, addr), IrType::Void);
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Dce.run(&mut m));
        assert_eq!(m.functions[0].blocks[0].insts.len(), 3);
    }

    #[test]
    fn cascades_through_chain() {
        // %1 = const 3
        // %2 = const 4
        // %3 = iadd %1, %2     ; unused
        // %4 = fmul ...        ; uses %3 transitively?  No — distinct chain
        // After DCE: only the terminator remains.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let a = push(
            &mut f,
            InstKind::ConstInt(3, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let b = push(
            &mut f,
            InstKind::ConstInt(4, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let c = push(&mut f, InstKind::IAdd(a, b), IrType::Int(IntWidth::I32));
        let _d = push(&mut f, InstKind::INeg(c), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dce.run(&mut m));
        assert!(m.functions[0].blocks[0].insts.is_empty());
    }

    #[test]
    fn keeps_call_even_if_result_unused() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        push(
            &mut f,
            InstKind::RuntimeCall(RuntimeFunc::PrintNewline, vec![]),
            IrType::Void,
        );
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Dce.run(&mut m));
        assert_eq!(m.functions[0].blocks[0].insts.len(), 1);
    }

    #[test]
    fn removes_unused_internal_pure_call() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new("pure_fn".into(), vec![], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let c = push(
            &mut callee,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(c)));
        m.add_function(callee);

        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        assert!(Dce.run(&mut m));
        assert!(
            m.functions[1].blocks[0].insts.is_empty(),
            "unused PURE call should be removed"
        );
    }

    #[test]
    fn keeps_unused_internal_call_when_pure_flag_hides_runtime_effect() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new("mislabelled_fn".into(), vec![], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let seven = push(
            &mut callee,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(
            &mut callee,
            InstKind::RuntimeCall(RuntimeFunc::PrintInt, vec![seven]),
            IrType::Void,
        );
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(seven)));
        m.add_function(callee);

        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        Dce.run(&mut m);
        assert_eq!(
            m.functions[1].blocks[0].insts.len(),
            1,
            "DCE must inspect the callee body instead of trusting is_pure"
        );
    }

    #[test]
    fn keeps_unused_internal_call_when_pure_flag_hides_external_call() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new("mislabelled_fn".into(), vec![], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let result = push(
            &mut callee,
            InstKind::Call(FuncRef::External("unknown_effect".into()), vec![]),
            IrType::Int(IntWidth::I32),
        );
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(result)));
        m.add_function(callee);

        assert!(
            !derive_internal_call_dce_info(&m)[0].discardable_when_unused,
            "an unknown external call must make the function observable"
        );
    }

    #[test]
    fn keeps_unused_internal_call_when_pure_flag_hides_nonlocal_store() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let param = Param {
            name: "out".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        };

        let mut callee = Function::new(
            "mislabelled_fn".into(),
            vec![param],
            IrType::Int(IntWidth::I32),
        );
        callee.is_pure = true;
        let one = push(
            &mut callee,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut callee, InstKind::Store(one, ValueId(0)), IrType::Void);
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(one)));
        m.add_function(callee);

        assert!(
            !derive_internal_call_dce_info(&m)[0].discardable_when_unused,
            "a store through a parameter must make the function observable"
        );
    }

    #[test]
    fn keeps_unused_internal_call_when_transitive_callee_has_effects() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut leaf = Function::new("leaf".into(), vec![], IrType::Int(IntWidth::I32));
        leaf.is_pure = true;
        let seven = push(
            &mut leaf,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(
            &mut leaf,
            InstKind::RuntimeCall(RuntimeFunc::PrintInt, vec![seven]),
            IrType::Void,
        );
        let leaf_entry = leaf.entry;
        leaf.block_mut(leaf_entry).terminator = Some(Terminator::Return(Some(seven)));
        m.add_function(leaf);

        let mut wrapper = Function::new("wrapper".into(), vec![], IrType::Int(IntWidth::I32));
        wrapper.is_pure = true;
        let result = push(
            &mut wrapper,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Int(IntWidth::I32),
        );
        let wrapper_entry = wrapper.entry;
        wrapper.block_mut(wrapper_entry).terminator = Some(Terminator::Return(Some(result)));
        m.add_function(wrapper);

        assert!(
            !derive_internal_call_dce_info(&m)[1].discardable_when_unused,
            "effects must propagate through internal PURE call chains"
        );
    }

    #[test]
    fn removes_unused_internal_pure_call_with_local_store() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new(
            "pure_local_store".into(),
            vec![],
            IrType::Int(IntWidth::I32),
        );
        callee.is_pure = true;
        let slot = push(
            &mut callee,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let zero = push(
            &mut callee,
            InstKind::ConstInt(0, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let element = push(
            &mut callee,
            InstKind::GetElementPtr(slot, vec![zero]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let seven = push(
            &mut callee,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut callee, InstKind::Store(seven, element), IrType::Void);
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(seven)));
        m.add_function(callee);

        assert!(
            derive_internal_call_dce_info(&m)[0].discardable_when_unused,
            "stores proven local to the callee must not disable PURE call removal"
        );
    }

    #[test]
    fn removes_unused_internal_pure_call_that_only_reads_global_state() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new(
            "pure_global_read".into(),
            vec![],
            IrType::Int(IntWidth::I32),
        );
        callee.is_pure = true;
        let addr = push(
            &mut callee,
            InstKind::GlobalAddr("module_state".into()),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let value = push(
            &mut callee,
            InstKind::Load(addr),
            IrType::Int(IntWidth::I32),
        );
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(value)));
        m.add_function(callee);

        assert!(
            derive_internal_call_dce_info(&m)[0].discardable_when_unused,
            "Fortran PURE functions may read module state without making a dead call observable"
        );
    }

    #[test]
    fn keeps_unused_internal_pure_void_call() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new("pure_hidden_result_fn".into(), vec![], IrType::Void);
        callee.is_pure = true;
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(None));
        m.add_function(callee);

        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Void,
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        assert!(!Dce.run(&mut m));
        assert_eq!(
            m.functions[1].blocks[0].insts.len(),
            1,
            "PURE void calls write hidden-result or intent-out storage and must stay live"
        );
    }

    #[test]
    fn keeps_dead_fp_operation_when_status_is_observed() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let one = push(
            &mut f,
            InstKind::ConstFloat(1.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let zero = push(
            &mut f,
            InstKind::ConstFloat(0.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let division = push(
            &mut f,
            InstKind::FDiv(one, zero),
            IrType::Float(FloatWidth::F64),
        );
        let divide_by_zero = push(
            &mut f,
            InstKind::ConstInt(2, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let raised = push(
            &mut f,
            InstKind::Call(
                FuncRef::External("afs_ieee_test_flag".into()),
                vec![divide_by_zero],
            ),
            IrType::Int(IntWidth::I32),
        );
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(raised)));
        m.add_function(f);

        assert!(
            !Dce.run(&mut m),
            "an unused division remains observable through IEEE_DIVIDE_BY_ZERO"
        );
        assert!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .any(|inst| inst.id == division),
            "DCE removed the only instruction that raises the observed flag"
        );
    }

    #[test]
    fn keeps_unused_pure_call_when_status_observes_callee_fp_operation() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new("divide".into(), vec![], IrType::Float(FloatWidth::F64));
        callee.is_pure = true;
        let one = push(
            &mut callee,
            InstKind::ConstFloat(1.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let zero = push(
            &mut callee,
            InstKind::ConstFloat(0.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let quotient = push(
            &mut callee,
            InstKind::FDiv(one, zero),
            IrType::Float(FloatWidth::F64),
        );
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(quotient)));
        m.add_function(callee);

        let mut caller = Function::new("caller".into(), vec![], IrType::Int(IntWidth::I32));
        let call = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Float(FloatWidth::F64),
        );
        let divide_by_zero = push(
            &mut caller,
            InstKind::ConstInt(2, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let raised = push(
            &mut caller,
            InstKind::Call(
                FuncRef::External("afs_ieee_test_flag".into()),
                vec![divide_by_zero],
            ),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(raised)));
        m.add_function(caller);

        assert!(
            !Dce.run(&mut m),
            "a PURE call is not discardable when its FP status effect is observed"
        );
        assert!(
            m.functions[1].blocks[0]
                .insts
                .iter()
                .any(|inst| inst.id == call),
            "DCE removed the call that raises the observed flag"
        );
    }

    #[test]
    fn float_chain_is_dead_without_fp_environment_access() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let a = push(
            &mut f,
            InstKind::ConstFloat(1.5, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let b = push(
            &mut f,
            InstKind::ConstFloat(2.5, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let _ = push(&mut f, InstKind::FAdd(a, b), IrType::Float(FloatWidth::F64));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dce.run(&mut m));
        assert!(m.functions[0].blocks[0].insts.is_empty());
    }
}
