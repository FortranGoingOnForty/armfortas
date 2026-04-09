//! Dead Store Elimination.
//!
//! A store `Store(value, ptr)` is dead if `ptr` is written again
//! before it is read, with no intervening call that could observe the
//! stored value through that pointer.
//!
//! This pass performs a simple forward walk within each basic block.
//! It tracks the most recent store to each alloca address and marks
//! earlier stores as dead when the same address is overwritten.
//!
//! ### Scope
//!
//! * Operates within a single basic block (intraprocedural, no
//!   dataflow across block boundaries).
//! * Works on exact pointer identity (`ValueId` equality). Two GEPs
//!   to the same base at the same offset are distinct values and are
//!   not deduplicated — that requires alias analysis which is deferred.
//! * Conservative on calls: any call that receives a pointer argument
//!   flushes the pending store for that pointer (the call might read it).
//!   Calls with no pointer args leave pending stores intact.
//! * `Gep(base, ..)` uses `base` — flushes pending store for `base`
//!   because the GEP result might be used for a load.
//!
//! ### Effect
//!
//! Primarily removes double-stores to scalar allocas that mem2reg did
//! not promote (e.g., `character` kind locals, variables whose address
//! escapes through a GEP that is later GEP'd again). Also cleans up
//! initialization sequences where a variable is zeroed and then
//! immediately written with the real value.

use super::pass::Pass;
use crate::ir::inst::*;
use std::collections::{HashMap, HashSet};

/// Eliminate dead stores within a single basic block.
/// Returns true if any instructions were removed.
fn dse_block(block: &mut BasicBlock) -> bool {
    // pending: alloca/ptr ValueId → index of the most recent Store to it
    // that has not yet been read.
    let mut pending: HashMap<ValueId, usize> = HashMap::new();
    let mut dead: HashSet<usize> = HashSet::new();

    for (i, inst) in block.insts.iter().enumerate() {
        match &inst.kind {
            InstKind::Store(_, ptr) => {
                // If there is already a pending (unread) store to the same
                // address, the previous store is now dead.
                if let Some(prev) = pending.get(ptr).copied() {
                    dead.insert(prev);
                }
                pending.insert(*ptr, i);
            }

            InstKind::Load(ptr) => {
                // A load reads the pending store — it is no longer dead.
                pending.remove(ptr);
            }

            InstKind::GetElementPtr(base, _) => {
                // GEP exposes the base address to potential loads through the
                // derived pointer. Conservatively flush the pending store for
                // base to avoid incorrectly removing it.
                pending.remove(base);
            }

            InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
                // Flush any pointer arg that might be read by the call.
                for arg in args {
                    pending.remove(arg);
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
    for block in &mut func.blocks {
        if dse_block(block) {
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

    /// GEP on the alloca address should flush the pending store for that alloca.
    #[test]
    fn gep_flushes_pending_store() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 4);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let ptr = push(&mut f, InstKind::Alloca(arr_ty), arr_ptr_ty);
        let v   = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), i32_ty());
        push(&mut f, InstKind::Store(v, ptr), IrType::Void);  // pending
        let off = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), i32_ty());
        // GEP flushes the pending store for `ptr`
        push(&mut f, InstKind::GetElementPtr(ptr, vec![off]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        // Store should NOT be removed (GEP flushed the pending entry).
        assert!(!Dse.run(&mut m));
        let stores: Vec<_> = m.functions[0].blocks[0].insts.iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .collect();
        assert_eq!(stores.len(), 1, "store must survive — GEP indicates a read path");
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
