//! Dead-code elimination pass.
//!
//! Removes instructions whose result has no observable effect:
//!
//!  * The result `ValueId` is unused by any other instruction or
//!    terminator in the function.
//!  * The instruction has no side effects of its own (a `Store`,
//!    `Call`, or `RuntimeCall` is always live, even if its return is
//!    unused, because it can write memory or perform I/O).
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
//!  * `Return` / branches / `Unreachable` — terminators are *always*
//!    live; we never remove them.
//!
//! `Load` is conservatively pure here. That is technically incorrect
//! when the same address is later stored to (the load could be
//! observing volatile memory), but for SSA-form Fortran loads of
//! locals it's a safe approximation. A future alias-analysis pass can
//! refine this.

use super::pass::Pass;
use super::util::{inst_uses, terminator_uses, prune_unreachable};
use crate::ir::inst::*;
use std::collections::{HashMap, HashSet};

/// True if the instruction has any side effect that prevents removal,
/// regardless of whether its result is used.
fn has_side_effect(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::Store(..)
            | InstKind::Call(..)
            | InstKind::RuntimeCall(..)
            // Alloca produces a stack slot whose identity matters even
            // if no one reads from it (the address may escape via a
            // future store/call). Treat as side-effecting for safety.
            | InstKind::Alloca(..)
    )
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
fn dce_function(func: &mut Function) -> bool {
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
                    if has_side_effect(&inst.kind) { return true; }
                    if live.contains(&inst.id) { return true; }
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
    if prune_unreachable(func) { any_change = true; }
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
        if block.id == func.entry { continue; }
        for (idx, p) in block.params.iter().enumerate() {
            if !live.contains(&p.id) {
                by_block.entry(block.id).or_default().push(idx);
            }
        }
    }
    if by_block.is_empty() { return false; }

    // Map BlockId → vec index. LICM/DCE never structurally reorder
    // `func.blocks`; only the inst vectors change. So this stays
    // valid for the lifetime of this call.
    let block_index: HashMap<BlockId, usize> = func.blocks.iter()
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
            debug_assert!(idx < args.len(),
                "drop_branch_arg: branch arg list out of sync with target params");
            if idx < args.len() { args.remove(idx); }
        }
        Terminator::CondBranch { true_dest, true_args, false_dest, false_args, .. } => {
            if *true_dest == target {
                debug_assert!(idx < true_args.len(),
                    "drop_branch_arg: cond_branch true_args out of sync");
                if idx < true_args.len() { true_args.remove(idx); }
            }
            if *false_dest == target {
                debug_assert!(idx < false_args.len(),
                    "drop_branch_arg: cond_branch false_args out of sync");
                if idx < false_args.len() { false_args.remove(idx); }
            }
        }
        Terminator::Switch { cases, default, .. } => {
            // Switch targets cannot have block params per the
            // verifier (`check_branch_args` rejects them). Audit
            // M-6: if we ever see a Switch that branches into a
            // block we're stripping params from, that's a real IR
            // validity bug. Assert instead of silently skipping.
            let touches_target = cases.iter().any(|(_, b)| *b == target)
                || *default == target;
            debug_assert!(!touches_target,
                "drop_branch_arg: Switch terminator branches to a block with params \
                 — verifier should reject this construction");
        }
        _ => {}
    }
}

pub struct Dce;

impl Pass for Dce {
    fn name(&self) -> &'static str { "dce" }
    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if dce_function(func) { changed = true; }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth, FloatWidth};
    use crate::lexer::{Span, Position};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span { start: p, end: p, file_id: 0 }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst { id, kind, ty, span: dummy_span() });
        id
    }

    #[test]
    fn removes_unused_const() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dce.run(&mut m));
        assert!(m.functions[0].blocks[0].insts.is_empty());
    }

    #[test]
    fn keeps_const_used_by_return() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let v = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(v)));
        m.add_function(f);

        assert!(!Dce.run(&mut m));
        assert_eq!(m.functions[0].blocks[0].insts.len(), 1);
    }

    #[test]
    fn keeps_store_even_if_address_unused() {
        // Alloca + Store should both stay even though no one loads.
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let addr = push(&mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let v = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
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
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let a = push(&mut f, InstKind::ConstInt(3, IntWidth::I32), IrType::Int(IntWidth::I32));
        let b = push(&mut f, InstKind::ConstInt(4, IntWidth::I32), IrType::Int(IntWidth::I32));
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
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        push(&mut f,
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
    fn float_chain_is_dead() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let a = push(&mut f, InstKind::ConstFloat(1.5, FloatWidth::F64), IrType::Float(FloatWidth::F64));
        let b = push(&mut f, InstKind::ConstFloat(2.5, FloatWidth::F64), IrType::Float(FloatWidth::F64));
        let _ = push(&mut f, InstKind::FAdd(a, b), IrType::Float(FloatWidth::F64));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dce.run(&mut m));
        assert!(m.functions[0].blocks[0].insts.is_empty());
    }
}
