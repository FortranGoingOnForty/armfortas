//! Alias analysis — Fortran-specific oracle.
//!
//! Determines whether two memory pointers can refer to the same
//! storage location. Leverages Fortran's strong aliasing guarantees:
//!
//! - Distinct `Alloca` instructions → NoAlias (different stack slots)
//! - Distinct `GlobalAddr` names → NoAlias (Fortran no-alias guarantee)
//! - Same pointer value → MustAlias
//! - GEP from same base with different constant offsets → NoAlias
//! - Everything else → MayAlias (conservative)
//!
//! Used by GVN (to determine if a store kills a prior load's value)
//! and cross-block load-store forwarding.

use crate::ir::inst::*;
use super::loop_utils::resolve_const_int;

/// Result of an alias query between two pointer values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasResult {
    /// The pointers definitely refer to the same location.
    MustAlias,
    /// The pointers might refer to the same location.
    MayAlias,
    /// The pointers definitely refer to different locations.
    NoAlias,
}

/// Query whether two pointer values may alias.
///
/// Both `a` and `b` should be pointer-typed values (results of Alloca,
/// GlobalAddr, GetElementPtr, or function parameters).
pub fn query(func: &Function, a: ValueId, b: ValueId) -> AliasResult {
    // Same value → must alias.
    if a == b { return AliasResult::MustAlias; }

    // Trace both pointers to their base + offset.
    let base_a = trace_base(func, a);
    let base_b = trace_base(func, b);

    // Different base pointers → no alias (Fortran guarantee).
    match (&base_a, &base_b) {
        (PtrBase::Alloca(id_a), PtrBase::Alloca(id_b)) => {
            if id_a != id_b { return AliasResult::NoAlias; }
        }
        (PtrBase::Global(name_a), PtrBase::Global(name_b)) => {
            if name_a != name_b { return AliasResult::NoAlias; }
        }
        (PtrBase::Alloca(_), PtrBase::Global(_)) |
        (PtrBase::Global(_), PtrBase::Alloca(_)) => {
            return AliasResult::NoAlias;
        }
        _ => {}
    }

    // Same base, different constant offsets → no alias.
    if base_a.base_id() == base_b.base_id() {
        let off_a = trace_offset(func, a);
        let off_b = trace_offset(func, b);
        if let (Some(oa), Some(ob)) = (off_a, off_b) {
            if oa != ob { return AliasResult::NoAlias; }
            // Same base + same offset → must alias.
            return AliasResult::MustAlias;
        }
    }

    AliasResult::MayAlias
}

/// Traced pointer base — the root allocation or global.
#[derive(Debug)]
enum PtrBase {
    Alloca(ValueId),
    Global(String),
    Param(ValueId),
    Unknown,
}

impl PtrBase {
    fn base_id(&self) -> Option<ValueId> {
        match self {
            PtrBase::Alloca(id) | PtrBase::Param(id) => Some(*id),
            _ => None,
        }
    }
}

/// Trace a pointer value back to its base allocation.
fn trace_base(func: &Function, ptr: ValueId) -> PtrBase {
    // Check if this is a function parameter (pointer arg).
    for param in &func.params {
        if param.id == ptr { return PtrBase::Param(ptr); }
    }

    // Find the defining instruction.
    let Some(inst) = find_inst(func, ptr) else {
        return PtrBase::Unknown;
    };

    match &inst.kind {
        InstKind::Alloca(_) => PtrBase::Alloca(ptr),
        InstKind::GlobalAddr(name) => PtrBase::Global(name.clone()),
        InstKind::GetElementPtr(base, _) => trace_base(func, *base),
        InstKind::Load(_) => PtrBase::Unknown, // loaded pointer — can't trace
        _ => PtrBase::Unknown,
    }
}

/// Trace a pointer to its constant byte offset from the base, if possible.
fn trace_offset(func: &Function, ptr: ValueId) -> Option<i64> {
    let inst = find_inst(func, ptr)?;
    match &inst.kind {
        InstKind::Alloca(_) | InstKind::GlobalAddr(_) => Some(0),
        InstKind::GetElementPtr(base, indices) => {
            let base_offset = trace_offset(func, *base)?;
            if indices.len() != 1 { return None; }
            let idx = resolve_const_int(func, indices[0])?;
            Some(base_offset + idx)
        }
        _ => None,
    }
}

fn find_inst(func: &Function, vid: ValueId) -> Option<&Inst> {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.id == vid { return Some(inst); }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::lexer::{Span, Position};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span { file_id: 0, start: pos, end: pos }
    }

    #[test]
    fn distinct_allocas_no_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let a = f.next_value_id();
        f.register_type(a, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: a, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let b = f.next_value_id();
        f.register_type(b, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: b, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(query(&f, a, b), AliasResult::NoAlias);
    }

    #[test]
    fn same_pointer_must_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let a = f.next_value_id();
        f.register_type(a, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: a, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(query(&f, a, a), AliasResult::MustAlias);
    }

    #[test]
    fn gep_different_offsets_no_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let base = f.next_value_id();
        f.register_type(base, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: base, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 10)),
        });
        // GEP at offset 0
        let c0 = f.next_value_id();
        f.register_type(c0, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c0, ty: IrType::Int(IntWidth::I64), span: span(),
            kind: InstKind::ConstInt(0, IntWidth::I64),
        });
        let gep0 = f.next_value_id();
        f.register_type(gep0, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep0, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::GetElementPtr(base, vec![c0]),
        });
        // GEP at offset 1
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c1, ty: IrType::Int(IntWidth::I64), span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let gep1 = f.next_value_id();
        f.register_type(gep1, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep1, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::GetElementPtr(base, vec![c1]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(query(&f, gep0, gep1), AliasResult::NoAlias);
    }

    #[test]
    fn distinct_globals_no_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let ga = f.next_value_id();
        f.register_type(ga, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: ga, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::GlobalAddr("array_a".into()),
        });
        let gb = f.next_value_id();
        f.register_type(gb, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gb, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::GlobalAddr("array_b".into()),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(query(&f, ga, gb), AliasResult::NoAlias);
    }
}
