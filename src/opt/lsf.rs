//! Local load-store forwarding pass (O1+).
//!
//! Within a single basic block, if a `load %ptr` immediately follows
//! (with no intervening store to `%ptr` or call that might write `%ptr`)
//! a `store %val, %ptr`, the load is redundant — we already know the
//! value is `%val`. This pass:
//!
//! 1. Tracks the most recently stored value for each address (`ValueId`).
//! 2. On a matching `load`, records `load_id → stored_val` in a
//!    substitution table.
//! 3. After scanning the block, rewrites every instruction that uses any
//!    forwarded `load_id` to use `stored_val` instead.
//!
//! The `load` instructions themselves become dead after substitution; the
//! subsequent DCE pass removes them.
//!
//! ### Alias model
//!
//! We use *structural pointer identity*: two addresses are considered the
//! same if and only if they have the same `ValueId`. This is always sound
//! (no false forwarding) but may miss opportunities when two independent
//! GEPs produce the same address. Alias analysis (deferred to sprint 29.8)
//! would improve coverage.
//!
//! ### Invalidation rules (conservative)
//!
//! | Instruction         | Effect on `available` map                    |
//! |---------------------|----------------------------------------------|
//! | `store %v, %ptr`    | `available[ptr] = v`                         |
//! | `load %ptr`         | Forward if available; otherwise no-op        |
//! | `call / rcall`      | Flush entire map (may write any pointer arg) |
//! | GEP of `%ptr`       | Flush `%ptr` (derived pointer may escape)    |
//! | Any other           | No-op (reads only)                           |
//!
//! ### Example
//!
//! ```text
//! ; Before LSF
//! store %42, %alloca
//! %w = load %alloca      ; forward → %w is replaced by %42
//! %x = iadd %w, %1      ; becomes iadd %42, %1
//!
//! ; After LSF + DCE
//! store %42, %alloca
//! %x = iadd %42, %1
//! ```

use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::{for_each_operand_mut, for_each_terminator_operand_mut};
use std::collections::HashMap;

pub struct LocalLsf;

impl Pass for LocalLsf {
    fn name(&self) -> &'static str { "load-store-fwd" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            changed |= lsf_in_function(func);
        }
        changed
    }
}

fn lsf_in_function(func: &mut Function) -> bool {
    let mut all_rewrites: HashMap<ValueId, ValueId> = HashMap::new();
    let mut changed = false;

    for block in &func.blocks {
        // available: ptr ValueId → the value most recently stored to it
        let mut available: HashMap<ValueId, ValueId> = HashMap::new();

        for inst in &block.insts {
            match &inst.kind {
                InstKind::Store(val, ptr) => {
                    available.insert(*ptr, *val);
                }

                InstKind::Load(ptr) => {
                    // Resolve the pointer through any prior substitutions so we
                    // check the forwarded pointer if it was itself renamed.
                    let eff_ptr = resolve(&all_rewrites, *ptr);
                    if let Some(&fwd) = available.get(&eff_ptr) {
                        // Forward: load_id → stored_value
                        all_rewrites.insert(inst.id, fwd);
                        changed = true;
                    }
                }

                InstKind::GetElementPtr(base, _) => {
                    // A GEP exposes `base` — conservatively kill any pending
                    // forwarding through `base` in case the GEP result is used
                    // to store before the next load of `base`.
                    available.remove(base);
                }

                InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
                    // Any call may write to any pointer arg. For simplicity,
                    // flush ALL available entries when a call is present.
                    //
                    // Refinement: only flush entries for which the address
                    // appears as a call argument.  The per-arg flush would be:
                    //   for arg in args { available.remove(arg); }
                    // But without alias analysis, the call might write through
                    // a saved copy of an arg that we can't see, so full flush
                    // is safer for an O1 pass. We can relax at O2+ with alias
                    // analysis.
                    let _ = args;
                    available.clear();
                }

                _ => {} // Pure computation — doesn't affect memory availability.
            }
        }
    }

    if all_rewrites.is_empty() {
        return false;
    }

    // Apply the substitution across the whole function (both instructions and
    // terminators, including block-param arguments on branches).
    apply_rewrites(func, &all_rewrites);
    changed
}

/// Follow a substitution chain to its canonical root.
///
/// Handles the case where forwarded loads are themselves forwarded again:
///   store %v, %p; %a = load %p;  ← forward: a → v
///   store %a, %q; %b = load %q;  ← forward: b → a, resolved → v
fn resolve(rewrites: &HashMap<ValueId, ValueId>, mut v: ValueId) -> ValueId {
    let mut steps = 0usize;
    while let Some(&next) = rewrites.get(&v) {
        v = next;
        steps += 1;
        if steps > 64 { break; } // cycle guard (SSA has none, but be safe)
    }
    v
}

/// Rewrite all uses of forwarded loads across the entire function.
///
/// Uses the same `for_each_operand_mut` / `for_each_terminator_operand_mut`
/// helpers as CSE to avoid code duplication.
fn apply_rewrites(func: &mut Function, rewrites: &HashMap<ValueId, ValueId>) {
    let r = |v: &mut ValueId| {
        let resolved = resolve(rewrites, *v);
        if resolved != *v {
            *v = resolved;
        }
    };
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            for_each_operand_mut(&mut inst.kind, r);
        }
        if let Some(term) = &mut block.terminator {
            for_each_terminator_operand_mut(term, r);
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;
    use crate::lexer::{Position, Span};

    fn dummy_span() -> Span {
        let p = Position { line: 0, col: 0 };
        Span { file_id: 0, start: p, end: p }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst { id, kind, ty, span: dummy_span() });
        id
    }

    #[test]
    fn forwards_load_after_store() {
        // store %42, %alloca
        // %w = load %alloca       ← should become %42
        // %x = iadd %w, %1       ← becomes iadd %42, %1
        let mut m = Module::new("test".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(&mut f, InstKind::Alloca(IrType::Int(IntWidth::I32)),
                          IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        let val42  = push(&mut f, InstKind::ConstInt(42, IntWidth::I32),
                          IrType::Int(IntWidth::I32));
        let one    = push(&mut f, InstKind::ConstInt(1, IntWidth::I32),
                          IrType::Int(IntWidth::I32));
        push(&mut f, InstKind::Store(val42, alloca), IrType::Void);
        let load   = push(&mut f, InstKind::Load(alloca), IrType::Int(IntWidth::I32));
        let add    = push(&mut f, InstKind::IAdd(load, one), IrType::Int(IntWidth::I32));
        let entry  = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(add)));
        m.add_function(f);

        let pass = LocalLsf;
        let changed = pass.run(&mut m);
        assert!(changed, "LSF should forward the load");

        // After forwarding, the IAdd should use val42 directly, not load.
        let func = &m.functions[0];
        let insts = &func.block(func.entry).insts;
        let add_inst = insts.iter().find(|i| i.id == add).unwrap();
        assert!(
            matches!(&add_inst.kind, InstKind::IAdd(a, _) if *a == val42),
            "IAdd operand should be forwarded to val42, got {:?}", add_inst.kind
        );
    }

    #[test]
    fn does_not_forward_across_intervening_store() {
        // store %42, %alloca
        // store %99, %alloca     ← overwrites the 42
        // %w = load %alloca      ← should forward to %99, not %42
        let mut m = Module::new("test".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(&mut f, InstKind::Alloca(IrType::Int(IntWidth::I32)),
                          IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        let v42 = push(&mut f, InstKind::ConstInt(42, IntWidth::I32),
                       IrType::Int(IntWidth::I32));
        let v99 = push(&mut f, InstKind::ConstInt(99, IntWidth::I32),
                       IrType::Int(IntWidth::I32));
        push(&mut f, InstKind::Store(v42, alloca), IrType::Void);
        push(&mut f, InstKind::Store(v99, alloca), IrType::Void);
        let load = push(&mut f, InstKind::Load(alloca), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        let pass = LocalLsf;
        let changed = pass.run(&mut m);
        assert!(changed, "LSF should forward to the latest store (99)");

        let func = &m.functions[0];
        let term = func.block(func.entry).terminator.as_ref().unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if *v == v99),
            "Return should use v99 (the latest store), got {:?}", term
        );
    }

    #[test]
    fn does_not_forward_across_call() {
        // store %42, %alloca
        // call @ext()            ← may write %alloca
        // %w = load %alloca      ← must NOT be forwarded
        let mut m = Module::new("test".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(&mut f, InstKind::Alloca(IrType::Int(IntWidth::I32)),
                          IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        let v42 = push(&mut f, InstKind::ConstInt(42, IntWidth::I32),
                       IrType::Int(IntWidth::I32));
        push(&mut f, InstKind::Store(v42, alloca), IrType::Void);
        // A call that might write to the alloca.
        push(&mut f, InstKind::Call(
            FuncRef::External("ext".into()), vec![alloca]
        ), IrType::Void);
        let load = push(&mut f, InstKind::Load(alloca), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        let pass = LocalLsf;
        // No forwarding should happen (call killed the available store).
        let changed = pass.run(&mut m);
        assert!(!changed, "LSF must not forward across a call");
    }

    #[test]
    fn no_forwarding_without_prior_store() {
        // %w = load %alloca      ← no prior store — nothing to forward
        let mut m = Module::new("test".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(&mut f, InstKind::Alloca(IrType::Int(IntWidth::I32)),
                          IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        let load = push(&mut f, InstKind::Load(alloca), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        let pass = LocalLsf;
        let changed = pass.run(&mut m);
        assert!(!changed, "No forwarding without a prior store");
    }
}
