//! Local load-store forwarding pass (O1+).
//!
//! Within a single basic block, if a `load %ptr` immediately follows
//! (with no intervening store to `%ptr` or call that might write `%ptr`)
//! a `store %val, %ptr`, the load is redundant — we already know the
//! value is `%val`. This pass:
//!
//! 1. Tracks the most recently stored value for each address.
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
//! Uses the Fortran-aware alias oracle:
//! - `MustAlias` store → later load can forward
//! - `MayAlias` store → pending forwarded values are conservatively killed
//! - `NoAlias` store/call arg → pending forwarded values survive
//!
//! ### Invalidation rules (conservative)
//!
//! | Instruction         | Effect on `available` map                    |
//! |---------------------|----------------------------------------------|
//! | `store %v, %ptr`    | kill aliasing entries; record `%ptr → v`     |
//! | `load %ptr`         | Forward if available; otherwise no-op        |
//! | `call / rcall`      | Flush only entries aliasing pointer args     |
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
use super::alias::{self, AliasResult};
use crate::ir::inst::*;
use crate::ir::types::IrType;
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
    #[derive(Clone, Copy)]
    struct AvailableStore {
        ptr: ValueId,
        val: ValueId,
    }

    let mut all_rewrites: HashMap<ValueId, ValueId> = HashMap::new();
    let mut changed = false;

    for block in &func.blocks {
        let mut available: Vec<AvailableStore> = Vec::new();

        for inst in &block.insts {
            match &inst.kind {
                InstKind::Store(val, ptr) => {
                    let eff_ptr = resolve(&all_rewrites, *ptr);
                    let eff_val = resolve(&all_rewrites, *val);
                    available.retain(|entry| {
                        matches!(alias::query(func, entry.ptr, eff_ptr), AliasResult::NoAlias)
                    });
                    available.push(AvailableStore { ptr: eff_ptr, val: eff_val });
                }

                InstKind::Load(ptr) => {
                    let eff_ptr = resolve(&all_rewrites, *ptr);
                    if let Some(entry) = available.iter().rev().find(|entry| {
                        matches!(alias::query(func, entry.ptr, eff_ptr), AliasResult::MustAlias)
                    }) {
                        all_rewrites.insert(inst.id, entry.val);
                        changed = true;
                    }
                }

                InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
                    let pointer_args: Vec<ValueId> = args
                        .iter()
                        .copied()
                        .map(|arg| resolve(&all_rewrites, arg))
                        .filter(|arg| value_is_pointer(func, *arg))
                        .collect();
                    if !pointer_args.is_empty() {
                        available.retain(|entry| {
                            pointer_args.iter().all(|arg| {
                                matches!(alias::query(func, entry.ptr, *arg), AliasResult::NoAlias)
                            })
                        });
                    }
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

fn value_is_pointer(func: &Function, value: ValueId) -> bool {
    if matches!(func.value_type(value), Some(IrType::Ptr(_))) {
        return true;
    }
    if func.params.iter().any(|param| param.id == value && matches!(param.ty, IrType::Ptr(_))) {
        return true;
    }
    func.blocks.iter().flat_map(|block| block.insts.iter()).find(|inst| inst.id == value)
        .map(|inst| matches!(inst.ty, IrType::Ptr(_)))
        .unwrap_or(false)
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

    fn param(name: &str, id: u32, fortran_noalias: bool) -> Param {
        Param {
            name: name.into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(id),
            fortran_noalias,
        }
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

    #[test]
    fn forwards_load_from_must_alias_gep() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 4);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let ptr = push(&mut f, InstKind::Alloca(arr_ty), arr_ptr_ty);
        let zero0 = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        let zero1 = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        let gep0 = push(
            &mut f,
            InstKind::GetElementPtr(ptr, vec![zero0]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
        );
        let gep1 = push(
            &mut f,
            InstKind::GetElementPtr(ptr, vec![zero1]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
        );
        let v42 = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        push(&mut f, InstKind::Store(v42, gep0), IrType::Void);
        let load = push(&mut f, InstKind::Load(gep1), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        assert!(LocalLsf.run(&mut m), "LSF should forward across must-alias GEPs");
        let term = m.functions[0].block(m.functions[0].entry).terminator.as_ref().unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if *v == v42),
            "return should use the stored value after forwarding, got {:?}",
            term
        );
    }

    #[test]
    fn keeps_store_available_across_noalias_call_arg() {
        let mut m = Module::new("test".into());
        let mut f = Function::new(
            "f".into(),
            vec![param("a", 0, true), param("b", 1, true)],
            IrType::Int(IntWidth::I32),
        );

        let v42 = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        push(&mut f, InstKind::Store(v42, ValueId(0)), IrType::Void);
        push(
            &mut f,
            InstKind::Call(FuncRef::External("touch".into()), vec![ValueId(1)]),
            IrType::Void,
        );
        let load = push(&mut f, InstKind::Load(ValueId(0)), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        assert!(
            LocalLsf.run(&mut m),
            "LSF should preserve the stored value across a noalias pointer call"
        );
        let term = m.functions[0].block(m.functions[0].entry).terminator.as_ref().unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if *v == v42),
            "return should use the forwarded value across the noalias call, got {:?}",
            term
        );
    }
}
