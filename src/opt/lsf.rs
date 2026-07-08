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

use super::alias::{self, AliasResult};
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::{for_each_operand_mut, for_each_terminator_operand_mut};
use std::collections::HashMap;

pub struct LocalLsf;

impl Pass for LocalLsf {
    fn name(&self) -> &'static str {
        "load-store-fwd"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            changed |= lsf_in_function(func, module.layout);
        }
        changed
    }
}

fn lsf_in_function(func: &mut Function, layout: crate::target::TargetLayout) -> bool {
    #[derive(Clone, Copy)]
    struct AvailableStore {
        ptr: ValueId,
        val: ValueId,
    }

    let mut all_rewrites: HashMap<ValueId, ValueId> = HashMap::new();
    let mut changed = false;

    {
        let mut alias_oracle = alias::AliasOracle::new(func, layout);

        for block in &func.blocks {
            let mut available: Vec<AvailableStore> = Vec::new();

            for inst in &block.insts {
                match &inst.kind {
                    InstKind::Store(val, ptr) => {
                        let eff_ptr = resolve(&all_rewrites, *ptr);
                        let eff_val = resolve(&all_rewrites, *val);
                        available.retain(|entry| {
                            matches!(alias_oracle.query(entry.ptr, eff_ptr), AliasResult::NoAlias)
                        });
                        available.push(AvailableStore {
                            ptr: eff_ptr,
                            val: eff_val,
                        });
                    }

                    InstKind::Load(ptr) => {
                        let eff_ptr = resolve(&all_rewrites, *ptr);
                        if let Some(entry) = available.iter().rev().find(|entry| {
                            matches!(
                                alias_oracle.query(entry.ptr, eff_ptr),
                                AliasResult::MustAlias
                            ) && func.value_type(entry.val).is_some_and(|ty| ty == inst.ty)
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
                            .filter(|arg| alias_oracle.value_is_pointer(*arg))
                            .collect();
                        if !pointer_args.is_empty() {
                            if pointer_args
                                .iter()
                                .any(|arg| call_arg_may_carry_indirect_pointer(func, *arg))
                            {
                                available.clear();
                                continue;
                            }
                            // Call boundaries use the coarser
                            // may_reach_through_call_arg predicate: a
                            // callee that gets a pointer can walk to
                            // any offset within the same allocation,
                            // so a precise "different GEP offset →
                            // NoAlias" answer is unsound here.
                            available.retain(|entry| {
                                pointer_args.iter().all(|arg| {
                                    !alias_oracle.may_reach_through_call_arg(entry.ptr, *arg)
                                })
                            });
                        }
                    }

                    _ => {} // Pure computation — doesn't affect memory availability.
                }
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
        if steps > 64 {
            break;
        } // cycle guard (SSA has none, but be safe)
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

fn call_arg_may_carry_indirect_pointer(func: &Function, value: ValueId) -> bool {
    matches!(
        func.value_type(value),
        Some(IrType::Ptr(inner))
            if matches!(
                inner.as_ref(),
                IrType::Array(..) | IrType::Struct(_) | IrType::Ptr(_) | IrType::FuncPtr(_)
            )
    )
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};
    use crate::opt::pass::Pass;

    fn dummy_span() -> Span {
        let p = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: p,
            end: p,
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
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(
            &mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let val42 = push(
            &mut f,
            InstKind::ConstInt(42, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let one = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(val42, alloca), IrType::Void);
        let load = push(&mut f, InstKind::Load(alloca), IrType::Int(IntWidth::I32));
        let add = push(
            &mut f,
            InstKind::IAdd(load, one),
            IrType::Int(IntWidth::I32),
        );
        let entry = f.entry;
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
            "IAdd operand should be forwarded to val42, got {:?}",
            add_inst.kind
        );
    }

    #[test]
    fn does_not_forward_across_intervening_store() {
        // store %42, %alloca
        // store %99, %alloca     ← overwrites the 42
        // %w = load %alloca      ← should forward to %99, not %42
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(
            &mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let v42 = push(
            &mut f,
            InstKind::ConstInt(42, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let v99 = push(
            &mut f,
            InstKind::ConstInt(99, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
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
            "Return should use v99 (the latest store), got {:?}",
            term
        );
    }

    #[test]
    fn does_not_forward_across_call() {
        // store %42, %alloca
        // call @ext()            ← may write %alloca
        // %w = load %alloca      ← must NOT be forwarded
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(
            &mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let v42 = push(
            &mut f,
            InstKind::ConstInt(42, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v42, alloca), IrType::Void);
        // A call that might write to the alloca.
        push(
            &mut f,
            InstKind::Call(FuncRef::External("ext".into()), vec![alloca]),
            IrType::Void,
        );
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
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let alloca = push(
            &mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
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
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 4);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let ptr = push(&mut f, InstKind::Alloca(arr_ty), arr_ptr_ty);
        let zero0 = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let zero1 = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
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
        let v42 = push(
            &mut f,
            InstKind::ConstInt(42, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v42, gep0), IrType::Void);
        let load = push(&mut f, InstKind::Load(gep1), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        assert!(
            LocalLsf.run(&mut m),
            "LSF should forward across must-alias GEPs"
        );
        let term = m.functions[0]
            .block(m.functions[0].entry)
            .terminator
            .as_ref()
            .unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if *v == v42),
            "return should use the stored value after forwarding, got {:?}",
            term
        );
    }

    #[test]
    fn does_not_forward_scalar_store_into_aggregate_load() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let arr_ty = IrType::Array(
            Box::new(IrType::Float(crate::ir::types::FloatWidth::F64)),
            2,
        );
        let alloca = push(
            &mut f,
            InstKind::Alloca(arr_ty.clone()),
            IrType::Ptr(Box::new(arr_ty.clone())),
        );
        let zero = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let elem_ptr = push(
            &mut f,
            InstKind::GetElementPtr(alloca, vec![zero]),
            IrType::Ptr(Box::new(IrType::Float(crate::ir::types::FloatWidth::F64))),
        );
        let scalar = push(
            &mut f,
            InstKind::ConstFloat(1.5, crate::ir::types::FloatWidth::F64),
            IrType::Float(crate::ir::types::FloatWidth::F64),
        );
        push(&mut f, InstKind::Store(scalar, elem_ptr), IrType::Void);
        let load = push(&mut f, InstKind::Load(alloca), arr_ty.clone());
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let changed = LocalLsf.run(&mut m);
        assert!(
            !changed,
            "LSF must not forward a scalar store into an aggregate-typed load"
        );

        let func = &m.functions[0];
        let load_inst = func
            .block(func.entry)
            .insts
            .iter()
            .find(|inst| inst.id == load)
            .expect("load should still exist");
        assert!(
            matches!(load_inst.kind, InstKind::Load(ptr) if ptr == alloca),
            "aggregate load should remain untouched, got {:?}",
            load_inst.kind
        );
    }

    #[test]
    fn call_invalidates_store_when_arg_shares_alloca_base_with_entry() {
        // A call receiving `gep %alloca, [0]` can walk to any
        // offset within %alloca; a prior store to `gep %alloca, [1]`
        // must be invalidated even though the precise offsets
        // differ.  Mirror of contained_dummy_array_ref: caller
        // stores arr(2)=20, calls touch(arr), then reads arr(2).
        // The read must see the callee's store, not the caller's
        // earlier constant.
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 2);
        let alloca = push(
            &mut f,
            InstKind::Alloca(arr_ty.clone()),
            IrType::Ptr(Box::new(arr_ty)),
        );
        let c0 = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let c1 = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let g0 = push(
            &mut f,
            InstKind::GetElementPtr(alloca, vec![c0]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let g1 = push(
            &mut f,
            InstKind::GetElementPtr(alloca, vec![c1]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let v20 = push(
            &mut f,
            InstKind::ConstInt(20, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v20, g1), IrType::Void);
        push(
            &mut f,
            InstKind::Call(FuncRef::External("touch".into()), vec![g0]),
            IrType::Void,
        );
        let load = push(&mut f, InstKind::Load(g1), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        LocalLsf.run(&mut m);
        let term = m.functions[0]
            .block(m.functions[0].entry)
            .terminator
            .as_ref()
            .unwrap();
        if let Terminator::Return(Some(v)) = term {
            assert_ne!(*v, v20, "LSF forwarded arr[1] load to the caller's pre-call constant across a call that received arr[0]; callee could have walked to arr[1]");
        } else {
            panic!("unexpected terminator: {:?}", term);
        }
    }

    #[test]
    fn chained_gep_through_arg_ptr_invalidates_direct_gep_store() {
        // Mirror of contained_dummy_array_ref at O1.
        //
        //   %alloca = alloca [i32 x 2]            ; caller's arr
        //   %g0     = gep %alloca, [0]            ; arr[0]
        //   %g1     = gep %alloca, [1]            ; arr[1]
        //   store %20, %g1                        ; arr[1] = 20 (caller init)
        //   %chain0 = gep %g0, [0]                ; chained arr[0]
        //   store %11, %chain0                    ; arr[0] = 11 (inlined touch)
        //   %chain1 = gep %g0, [1]                ; chained arr[1]
        //   store %31, %chain1                    ; arr[1] = 31 (inlined touch)
        //   %load   = load %g1                    ; print arr[1]
        //   return %load                          ; must be 31, not 20
        //
        // If LocalLsf or the alias oracle doesn't see that %chain1
        // aliases %g1 (both are byte-offset 4 from %alloca), the
        // load gets forwarded to %20 and the caller prints garbage.
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 2);
        let alloca = push(
            &mut f,
            InstKind::Alloca(arr_ty.clone()),
            IrType::Ptr(Box::new(arr_ty)),
        );
        let c0 = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let c1 = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let g0 = push(
            &mut f,
            InstKind::GetElementPtr(alloca, vec![c0]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let g1 = push(
            &mut f,
            InstKind::GetElementPtr(alloca, vec![c1]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );

        let v20 = push(
            &mut f,
            InstKind::ConstInt(20, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v20, g1), IrType::Void);

        let chain0 = push(
            &mut f,
            InstKind::GetElementPtr(g0, vec![c0]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let v11 = push(
            &mut f,
            InstKind::ConstInt(11, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v11, chain0), IrType::Void);

        let chain1 = push(
            &mut f,
            InstKind::GetElementPtr(g0, vec![c1]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let v31 = push(
            &mut f,
            InstKind::ConstInt(31, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v31, chain1), IrType::Void);

        let load = push(&mut f, InstKind::Load(g1), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        LocalLsf.run(&mut m);
        let term = m.functions[0]
            .block(m.functions[0].entry)
            .terminator
            .as_ref()
            .unwrap();
        // The load must either remain or be forwarded to v31 — never
        // to v20.
        if let Terminator::Return(Some(v)) = term {
            assert_ne!(*v, v20, "LSF forwarded load %g1 to the pre-chained-store value v20, shadowing the chained-GEP aliasing store v31");
        } else {
            panic!("unexpected terminator: {:?}", term);
        }
    }

    #[test]
    fn keeps_store_available_across_noalias_call_arg() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new(
            "f".into(),
            vec![param("a", 0, true), param("b", 1, true)],
            IrType::Int(IntWidth::I32),
        );

        let v42 = push(
            &mut f,
            InstKind::ConstInt(42, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(&mut f, InstKind::Store(v42, ValueId(0)), IrType::Void);
        push(
            &mut f,
            InstKind::Call(FuncRef::External("touch".into()), vec![ValueId(1)]),
            IrType::Void,
        );
        let load = push(
            &mut f,
            InstKind::Load(ValueId(0)),
            IrType::Int(IntWidth::I32),
        );
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        assert!(
            LocalLsf.run(&mut m),
            "LSF should preserve the stored value across a noalias pointer call"
        );
        let term = m.functions[0]
            .block(m.functions[0].entry)
            .terminator
            .as_ref()
            .unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if *v == v42),
            "return should use the forwarded value across the noalias call, got {:?}",
            term
        );
    }

    #[test]
    fn aggregate_call_arg_invalidates_store_reachable_through_wrapped_pointer() {
        // Mirrors type-bound calls through class descriptors:
        //
        //   store 42, %payload
        //   store %payload, %wrapper[0]
        //   call @touch(%wrapper)
        //   load %payload
        //
        // The call's direct argument is a different alloca from
        // %payload, but the aggregate wrapper carries %payload inside
        // it. LSF must not forward the pre-call value across this call.
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I64));

        let payload = push(
            &mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I64)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))),
        );
        let wrapper_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384);
        let wrapper = push(
            &mut f,
            InstKind::Alloca(wrapper_ty.clone()),
            IrType::Ptr(Box::new(wrapper_ty)),
        );
        let zero = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let wrapper_slot = push(
            &mut f,
            InstKind::GetElementPtr(wrapper, vec![zero]),
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))))),
        );
        let v42 = push(
            &mut f,
            InstKind::ConstInt(42, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        push(&mut f, InstKind::Store(v42, payload), IrType::Void);
        push(&mut f, InstKind::Store(payload, wrapper_slot), IrType::Void);
        push(
            &mut f,
            InstKind::Call(FuncRef::Internal(0), vec![wrapper]),
            IrType::Void,
        );
        let load = push(&mut f, InstKind::Load(payload), IrType::Int(IntWidth::I64));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        LocalLsf.run(&mut m);
        let term = m.functions[0]
            .block(m.functions[0].entry)
            .terminator
            .as_ref()
            .unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if *v == load),
            "LSF must not forward a payload load across a call receiving an aggregate wrapper, got {:?}",
            term
        );
    }
}
