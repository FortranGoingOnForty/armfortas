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
//! * Calls with no pointer args leave pending stores intact.
//!
//! ### Effect
//!
//! Primarily removes double-stores to scalar allocas that mem2reg did
//! not promote (e.g., `character` kind locals, variables whose address
//! escapes through a GEP that is later GEP'd again). Also cleans up
//! initialization sequences where a variable is zeroed and then
//! immediately written with the real value.

use super::pass::Pass;
use super::alias::{self, AliasResult};
use crate::ir::inst::*;
use crate::ir::types::IrType;
use std::collections::HashSet;

/// Eliminate dead stores within a single basic block.
/// Returns true if any instructions were removed.
fn dse_block(func: &mut Function, block_idx: usize) -> bool {
    #[derive(Clone, Copy)]
    struct PendingStore {
        ptr: ValueId,
        inst_idx: usize,
    }

    let block = &func.blocks[block_idx];
    let mut pending: Vec<PendingStore> = Vec::new();
    let mut dead: HashSet<usize> = HashSet::new();

    for (i, inst) in block.insts.iter().enumerate() {
        match &inst.kind {
            InstKind::Store(_, ptr) => {
                let mut next_pending = Vec::with_capacity(pending.len() + 1);
                for entry in pending {
                    match alias::query(func, entry.ptr, *ptr) {
                        AliasResult::MustAlias => {
                            dead.insert(entry.inst_idx);
                        }
                        AliasResult::MayAlias | AliasResult::NoAlias => {
                            next_pending.push(entry);
                        }
                    }
                }
                next_pending.push(PendingStore { ptr: *ptr, inst_idx: i });
                pending = next_pending;
            }

            InstKind::Load(ptr) => {
                pending.retain(|entry| {
                    matches!(alias::query(func, entry.ptr, *ptr), AliasResult::NoAlias)
                });
            }

            InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
                let pointer_args: Vec<ValueId> = args
                    .iter()
                    .copied()
                    .filter(|arg| {
                        matches!(
                            func.value_type(*arg),
                            Some(IrType::Ptr(_))
                        )
                    })
                    .collect();
                if !pointer_args.is_empty() {
                    pending.retain(|entry| {
                        pointer_args.iter().all(|arg| {
                            matches!(alias::query(func, entry.ptr, *arg), AliasResult::NoAlias)
                        })
                    });
                }
            }

            // Memset/memcpy wrappers (external calls) are handled above via Call.
            // All other instructions are pure reads of their operands — they
            // cannot modify memory so they don't affect pending stores.
            _ => {}
        }
    }
    // Note: remaining entries in `pending` at block exit are stores to locals
    // whose value has never been read in this block. We conservatively keep
    // them — they might be read in a successor block.  A full dataflow DSE
    // (liveness across blocks) could remove them, but is deferred.

    if dead.is_empty() {
        return false;
    }

    let block = &mut func.blocks[block_idx];
    let mut idx = 0usize;
    block.insts.retain(|_| {
        let keep = !dead.contains(&idx);
        idx += 1;
        keep
    });
    true
}

fn dse_function(func: &mut Function) -> bool {
    let mut changed = false;
    for block_idx in 0..func.blocks.len() {
        if dse_block(func, block_idx) {
            changed = true;
        }
    }
    changed
}

pub struct Dse;

impl Pass for Dse {
    fn name(&self) -> &'static str { "dse" }
    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if dse_function(func) { changed = true; }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
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

    fn ptr_ty() -> IrType { IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))) }
    fn alloca_ty() -> IrType { IrType::Int(IntWidth::I32) }
    fn i32_ty() -> IrType { IrType::Int(IntWidth::I32) }

    /// %ptr = alloca i32
    /// store 1, %ptr
    /// store 2, %ptr    ← first store is dead
    /// load  %ptr
    #[test]
    fn double_store_kills_first() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1  = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2  = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, ptr), IrType::Void);   // dead
        push(&mut f, InstKind::Store(v2, ptr), IrType::Void);   // live
        let _loaded = push(&mut f, InstKind::Load(ptr), i32_ty());
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dse.run(&mut m), "DSE should remove the dead store");
        // alloca, v1, v2, live_store, load = 5 remaining (store 1 gone)
        let insts = &m.functions[0].blocks[0].insts;
        assert_eq!(insts.len(), 5, "first store should be eliminated");
        // The surviving Store should use v2.
        let stores: Vec<_> = insts.iter().filter(|i| matches!(i.kind, InstKind::Store(..))).collect();
        assert_eq!(stores.len(), 1);
        assert!(matches!(stores[0].kind, InstKind::Store(v, _) if v == v2));
    }

    /// store then load: first store is live (must not be removed).
    #[test]
    fn store_then_load_is_live() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], i32_ty());
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1  = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), i32_ty());
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
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 4);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let ptr = push(&mut f, InstKind::Alloca(arr_ty), arr_ptr_ty);
        let off0 = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), i32_ty());
        let off1 = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), i32_ty());
        let gep0 = push(&mut f, InstKind::GetElementPtr(ptr, vec![off0]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
        let gep1 = push(&mut f, InstKind::GetElementPtr(ptr, vec![off1]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
        let v1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2 = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, gep0), IrType::Void);  // dead
        push(&mut f, InstKind::Store(v2, gep1), IrType::Void);  // live
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Dse.run(&mut m));
        let stores: Vec<_> = m.functions[0].blocks[0].insts.iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .collect();
        assert_eq!(stores.len(), 1, "must-alias GEP overwrite should kill the first store");
        assert!(matches!(stores[0].kind, InstKind::Store(v, _) if v == v2));
    }

    /// Three consecutive stores to the same address: only the last is live.
    #[test]
    fn triple_store_keeps_last() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let ptr = push(&mut f, InstKind::Alloca(alloca_ty()), ptr_ty());
        let v1  = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), i32_ty());
        let v2  = push(&mut f, InstKind::ConstInt(2, IntWidth::I32), i32_ty());
        let v3  = push(&mut f, InstKind::ConstInt(3, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v1, ptr), IrType::Void);  // dead
        push(&mut f, InstKind::Store(v2, ptr), IrType::Void);  // dead
        push(&mut f, InstKind::Store(v3, ptr), IrType::Void);  // live (block exit)
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        // After DSE: alloca + v1 + v2 + v3 + store(v3) = 5
        // (stores of v1 and v2 are dead; store of v3 remains because it
        //  might be read in a successor — we don't do cross-block analysis)
        assert!(Dse.run(&mut m));
        let stores: Vec<_> = m.functions[0].blocks[0].insts.iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .collect();
        // Two dead stores removed, one survives.
        assert_eq!(stores.len(), 1);
        assert!(matches!(stores[0].kind, InstKind::Store(v, _) if v == v3));
    }
}
