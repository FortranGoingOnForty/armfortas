//! Dead Store Elimination.
//!
//! A store `Store(value, ptr)` is dead if `ptr` is written again
//! before it is read, with no intervening call that could observe the
//! stored value through that pointer.
//!
//! This pass performs a simple forward walk within each basic block.
//! It tracks unread stores and marks earlier ones dead when a later
//! store is proven `MustAlias`-equivalent via alias analysis.
//!
//! ### Scope
//!
//! * Operates within a single basic block (intraprocedural, no
//!   dataflow across block boundaries).
//! * Uses alias analysis:
//!   - later `MustAlias` store kills the earlier unread store
//!   - `Load`/call pointer args flush any pending store that is not `NoAlias`
//! * Calls preserve only non-global stores proven unreachable through their
//!   pointer arguments.
//!
//! ### Effect
//!
//! Primarily removes double-stores to scalar allocas that mem2reg did
//! not promote (e.g., `character` kind locals, variables whose address
//! escapes through a GEP that is later GEP'd again). Also cleans up
//! initialization sequences where a variable is zeroed and then
//! immediately written with the real value.

use super::alias::{self, AliasResult, ProvenLocation};
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PendingStoreKey {
    location: ProvenLocation,
    value_type: Option<IrType>,
}

#[derive(Clone, Copy)]
struct PendingStore {
    ptr: ValueId,
    inst_idx: usize,
}

/// Find dead stores within one block. Store-to-store replacement uses an exact
/// location index; memory-observing barriers retain the conservative alias scan.
fn find_dead_stores(
    block: &BasicBlock,
    alias_oracle: &mut alias::AliasOracle<'_>,
) -> HashSet<usize> {
    let mut pending: HashMap<PendingStoreKey, PendingStore> = HashMap::new();
    let mut dead: HashSet<usize> = HashSet::new();

    for (i, inst) in block.insts.iter().enumerate() {
        match &inst.kind {
            InstKind::Store(value, ptr) => {
                let key = PendingStoreKey {
                    location: alias_oracle.proven_location(*ptr),
                    value_type: alias_oracle.value_type(*value).cloned(),
                };
                if let Some(overwritten) = pending.insert(
                    key,
                    PendingStore {
                        ptr: *ptr,
                        inst_idx: i,
                    },
                ) {
                    dead.insert(overwritten.inst_idx);
                }
            }

            InstKind::Load(ptr) => {
                pending.retain(|_, entry| {
                    matches!(alias_oracle.query(entry.ptr, *ptr), AliasResult::NoAlias)
                });
            }

            // VOLATILE accesses are explicit optimizer barriers. In addition
            // to preserving the access itself, assume asynchronously visible
            // storage may have changed at this point.
            InstKind::VolatileLoad(_) | InstKind::VolatileStore(..) => pending.clear(),

            // The alias oracle compares point addresses, while vector memory
            // operations cover a 16-byte range. Until range aliasing exists,
            // no pending scalar store is proven disjoint from that access.
            InstKind::VLoad(_) | InstKind::VStore(..) => pending.clear(),

            InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
                // Calls can observe module/global state without receiving
                // its address as an argument, so those stores are never
                // dead across a call boundary.
                pending.retain(|_, entry| !alias_oracle.requires_global_call_barrier(entry.ptr));
                // Call boundaries use the coarser predicate — same fix
                // as LocalLsf / GlobalLsf.  A callee that receives a
                // pointer into an allocation can walk to any offset
                // within it, so `gep %a,[0]` passed as an arg must
                // invalidate a pending store to `gep %a,[1]` even
                // though their precise offsets differ. Aggregate-like
                // pointees may also carry pointers to otherwise unrelated
                // allocations, requiring a full pending-store barrier.
                let pointer_args: Vec<ValueId> = args
                    .iter()
                    .copied()
                    .filter(|arg| alias_oracle.value_is_pointer(*arg))
                    .collect();
                if !pointer_args.is_empty() {
                    if pointer_args
                        .iter()
                        .any(|arg| call_arg_may_carry_indirect_pointer(alias_oracle, *arg))
                    {
                        pending.clear();
                        continue;
                    }
                    pending.retain(|_, entry| {
                        pointer_args
                            .iter()
                            .all(|arg| !alias_oracle.may_reach_through_call_arg(entry.ptr, *arg))
                    });
                }
            }

            // Memset/memcpy wrappers (external calls) are handled above via Call.
            // All other instructions are pure reads of their operands — they
            // cannot modify memory so they don't affect pending stores.
            _ => {}
        }
    }
    // Remaining entries at block exit might be read in a successor block, so
    // they stay live. Cross-block DSE requires a separate dataflow analysis.

    dead
}

fn call_arg_may_carry_indirect_pointer(
    alias_oracle: &alias::AliasOracle<'_>,
    value: ValueId,
) -> bool {
    matches!(
        alias_oracle.value_type(value),
        Some(IrType::Ptr(inner))
            if matches!(
                inner.as_ref(),
                IrType::Array(..) | IrType::Struct(_) | IrType::Ptr(_) | IrType::FuncPtr(_)
            )
    )
}

fn remove_dead_stores(block: &mut BasicBlock, dead: &HashSet<usize>) -> bool {
    if dead.is_empty() {
        return false;
    }

    let mut idx = 0usize;
    block.insts.retain(|_| {
        let keep = !dead.contains(&idx);
        idx += 1;
        keep
    });
    true
}

fn dse_function(func: &mut Function, layout: crate::target::TargetLayout) -> bool {
    let dead_by_block = {
        let func_ref: &Function = func;
        let mut alias_oracle = alias::AliasOracle::new(func_ref, layout);
        func_ref
            .blocks
            .iter()
            .map(|block| find_dead_stores(block, &mut alias_oracle))
            .collect::<Vec<_>>()
    };

    let mut changed = false;
    for (block, dead) in func.blocks.iter_mut().zip(&dead_by_block) {
        if remove_dead_stores(block, dead) {
            changed = true;
        }
    }
    changed
}

pub struct Dse;

impl Pass for Dse {
    fn name(&self) -> &'static str {
        "dse"
    }
    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if dse_function(func, module.layout) {
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
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

    fn ptr_ty() -> IrType {
        IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))
    }
    fn alloca_ty() -> IrType {
        IrType::Int(IntWidth::I32)
    }
    fn i32_ty() -> IrType {
        IrType::Int(IntWidth::I32)
    }

    /// %ptr = alloca i32
    /// store 1, %ptr
    /// store 2, %ptr    ← first store is dead
    /// load  %ptr
    #[test]
    fn double_store_kills_first() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, ptr), IrType::Void); // dead
        push(&mut f, InstKind::Store(v2, ptr), IrType::Void); // live
        let _loaded = push(&mut f, InstKind::Load(ptr), i32_ty());
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dse.run(&mut m), "DSE should remove the dead store");
        // alloca, v1, v2, live_store, load = 5 remaining (store 1 gone)
        let insts = &m.functions[0].blocks[0].insts;
        assert_eq!(insts.len(), 5, "first store should be eliminated");
        // The surviving Store should use v2.
        let stores: Vec<_> = insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .collect();
        assert_eq!(stores.len(), 1);
        assert!(matches!(stores[0].kind, InstKind::Store(v, _) if v == v2));
    }

    #[test]
    fn volatile_access_is_a_dead_store_barrier() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, ptr), IrType::Void);
        push(&mut f, InstKind::VolatileLoad(ptr), i32_ty());
        push(&mut f, InstKind::Store(v2, ptr), IrType::Void);
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Dse.run(&mut m));
        assert_eq!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::Store(..)))
                .count(),
            2
        );
    }

    #[test]
    fn does_not_remove_global_store_across_zero_argument_call() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let global = push(&mut f, InstKind::GlobalAddr("state".into()), ptr_ty());
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, global), IrType::Void);
        push(
            &mut f,
            InstKind::Call(FuncRef::External("observe_global".into()), vec![]),
            IrType::Void,
        );
        push(&mut f, InstKind::Store(v2, global), IrType::Void);
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(
            !Dse.run(&mut m),
            "DSE must preserve a global store observable by an intervening call"
        );
        assert_eq!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::Store(..)))
                .count(),
            2
        );
    }

    #[test]
    fn aggregate_call_arg_preserves_store_reachable_through_wrapped_pointer() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let payload = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let wrapper_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 16);
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
            IrType::Ptr(Box::new(ptr_ty())),
        );
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, payload), IrType::Void);
        push(&mut f, InstKind::Store(payload, wrapper_slot), IrType::Void);
        push(
            &mut f,
            InstKind::Call(FuncRef::External("observe_indirect".into()), vec![wrapper]),
            IrType::Void,
        );
        push(&mut f, InstKind::Store(v2, payload), IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(
            !Dse.run(&mut m),
            "DSE must preserve a store observable through a pointer carried by an aggregate"
        );
        assert_eq!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::Store(..)))
                .count(),
            3
        );
    }

    /// store then load: first store is live (must not be removed).
    #[test]
    fn store_then_load_is_live() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], i32_ty());
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1 = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, ptr), IrType::Void);
        let loaded = push(&mut f, InstKind::Load(ptr), i32_ty());
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(loaded)));
        m.add_function(f);

        assert!(!Dse.run(&mut m), "nothing should be removed");
        assert_eq!(m.functions[0].blocks[0].insts.len(), 4);
    }

    /// Same base + same offset through distinct GEP values still aliases.
    #[test]
    fn same_offset_geps_kill_earlier_store() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 4);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let ptr = push(&mut f, InstKind::Alloca(arr_ty), arr_ptr_ty);
        let off0 = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), i32_ty());
        let off1 = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), i32_ty());
        let gep0 = push(
            &mut f,
            InstKind::GetElementPtr(ptr, vec![off0]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
        );
        let gep1 = push(
            &mut f,
            InstKind::GetElementPtr(ptr, vec![off1]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
        );
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, gep0), IrType::Void); // dead
        push(&mut f, InstKind::Store(v2, gep1), IrType::Void); // live
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dse.run(&mut m));
        let stores: Vec<_> = m.functions[0].blocks[0]
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .collect();
        assert_eq!(
            stores.len(),
            1,
            "must-alias GEP overwrite should kill the first store"
        );
        assert!(matches!(stores[0].kind, InstKind::Store(v, _) if v == v2));
    }

    /// Three consecutive stores to the same address: only the last is live.
    #[test]
    fn triple_store_keeps_last() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        let v3 = push(&mut f, InstKind::ConstInt(3, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, ptr), IrType::Void); // dead
        push(&mut f, InstKind::Store(v2, ptr), IrType::Void); // dead
        push(&mut f, InstKind::Store(v3, ptr), IrType::Void); // live (block exit)
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        // After DSE: alloca + v1 + v2 + v3 + store(v3) = 5
        // (stores of v1 and v2 are dead; store of v3 remains because it
        //  might be read in a successor — we don't do cross-block analysis)
        assert!(Dse.run(&mut m));
        let stores: Vec<_> = m.functions[0].blocks[0]
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .collect();
        // Two dead stores removed, one survives.
        assert_eq!(stores.len(), 1);
        assert!(matches!(stores[0].kind, InstKind::Store(v, _) if v == v3));
    }

    #[test]
    fn vector_load_observes_pending_scalar_store() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let array = IrType::Array(Box::new(i32_ty()), 4);
        let base = push(
            &mut f,
            InstKind::Alloca(array.clone()),
            IrType::Ptr(Box::new(array)),
        );
        let zero = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let one = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let vector_ptr = push(&mut f, InstKind::GetElementPtr(base, vec![zero]), ptr_ty());
        let scalar_ptr = push(&mut f, InstKind::GetElementPtr(base, vec![one]), ptr_ty());
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, scalar_ptr), IrType::Void);
        push(
            &mut f,
            InstKind::VLoad(vector_ptr),
            IrType::Vector {
                lanes: 4,
                elem: Box::new(i32_ty()),
            },
        );
        push(&mut f, InstKind::Store(v2, scalar_ptr), IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Dse.run(&mut m));
        assert_eq!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::Store(..)))
                .count(),
            2
        );
    }

    #[test]
    fn vector_store_is_a_conservative_pending_store_barrier() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        let vector = push(
            &mut f,
            InstKind::VBroadcast(v1),
            IrType::Vector {
                lanes: 4,
                elem: Box::new(i32_ty()),
            },
        );
        push(&mut f, InstKind::Store(v1, ptr), IrType::Void);
        push(&mut f, InstKind::VStore(vector, ptr), IrType::Void);
        push(&mut f, InstKind::Store(v2, ptr), IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Dse.run(&mut m));
        assert_eq!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::Store(..)))
                .count(),
            2
        );
    }

    #[test]
    fn mixed_width_overwrite_does_not_kill_wider_store() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let byte_array = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 16);
        let base = push(
            &mut f,
            InstKind::Alloca(byte_array.clone()),
            IrType::Ptr(Box::new(byte_array)),
        );
        let zero = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let ptr_i64 = push(
            &mut f,
            InstKind::GetElementPtr(base, vec![zero]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))),
        );
        let ptr_i32 = push(
            &mut f,
            InstKind::GetElementPtr(base, vec![zero]),
            IrType::Ptr(Box::new(i32_ty())),
        );
        let wide = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let narrow = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(wide, ptr_i64), IrType::Void);
        push(&mut f, InstKind::Store(narrow, ptr_i32), IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Dse.run(&mut m));
        assert_eq!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst.kind, InstKind::Store(..)))
                .count(),
            2
        );
    }

    #[test]
    fn independent_stores_do_not_issue_pairwise_alias_queries() {
        const COUNT: u64 = 2048;

        let mut f = Function::new("store_scaling".into(), vec![], IrType::Void);
        let array = IrType::Array(Box::new(i32_ty()), COUNT);
        let base = push(
            &mut f,
            InstKind::Alloca(array.clone()),
            IrType::Ptr(Box::new(array)),
        );
        let value = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        for index in 0..COUNT {
            let offset = push(
                &mut f,
                InstKind::ConstInt(index as i128, IntWidth::I64),
                IrType::Int(IntWidth::I64),
            );
            let ptr = push(
                &mut f,
                InstKind::GetElementPtr(base, vec![offset]),
                ptr_ty(),
            );
            push(&mut f, InstKind::Store(value, ptr), IrType::Void);
        }
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));

        let mut oracle = alias::AliasOracle::new(&f, crate::target::TargetLayout::LP64);
        let dead = find_dead_stores(&f.blocks[0], &mut oracle);
        assert!(dead.is_empty());
        assert_eq!(
            oracle.query_count(),
            0,
            "independent stores must use the location index, not pairwise alias queries"
        );
    }
}
