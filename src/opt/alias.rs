//! Alias analysis — Fortran-specific oracle.
//!
//! Determines whether two memory pointers can refer to the same
//! storage location. Leverages Fortran's strong aliasing guarantees:
//!
//! - Distinct `Alloca` instructions → NoAlias (different stack slots)
//! - Distinct `GlobalAddr` names → NoAlias (Fortran no-alias guarantee)
//! - Distinct ordinary Fortran dummy arguments → NoAlias
//! - Same pointer value → MustAlias
//! - GEP from same base with different constant offsets → NoAlias
//! - Everything else → MayAlias (conservative)
//!
//! Used by GVN (to determine if a store kills a prior load's value)
//! cross-block load-store forwarding, and LICM load hoisting.

use super::loop_utils::resolve_const_int;
use crate::ir::inst::*;
use crate::ir::types::IrType;

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

/// True if `entry_ptr` may be reachable through `call_arg` when
/// a call passes `call_arg` across a function boundary.
///
/// At call boundaries the precise offset-based alias check is
/// unsound: the callee receives the pointer and can walk to any
/// offset within the same allocation.  `query()` would say
/// `NoAlias` for `gep %0, [0]` vs `gep %0, [1]` because their
/// offsets differ, but a callee reading through the former can
/// still touch the latter.  Load-store forwarding and DSE must
/// use this coarser predicate when reasoning about calls.
///
/// The rule: if both pointers trace to the same underlying
/// allocation (same alloca, same global, same param slot), they
/// are considered call-reachable aliases regardless of offset.
/// Distinct allocations remain NoAlias by Fortran's strong
/// aliasing guarantee.
pub fn may_reach_through_call_arg(
    func: &Function,
    entry_ptr: ValueId,
    call_arg: ValueId,
    layout: crate::target::TargetLayout,
) -> bool {
    if entry_ptr == call_arg {
        return true;
    }
    let base_entry = trace_base(func, entry_ptr, layout);
    let base_arg = trace_base(func, call_arg, layout);
    match (&base_entry, &base_arg) {
        (PtrBase::Alloca(a), PtrBase::Alloca(b)) => a == b,
        (PtrBase::Global(a), PtrBase::Global(b)) => a == b,
        (PtrBase::Param(a), PtrBase::Param(b)) => {
            if a == b {
                return true;
            }
            !(param_is_fortran_noalias(func, *a) && param_is_fortran_noalias(func, *b))
        }
        (PtrBase::Unknown, _) | (_, PtrBase::Unknown) => true,
        // Distinct kinds of allocations never alias per Fortran.
        _ => false,
    }
}

/// Query whether two pointer values may alias.
///
/// Both `a` and `b` should be pointer-typed values (results of Alloca,
/// GlobalAddr, GetElementPtr, or function parameters).
pub fn query(
    func: &Function,
    a: ValueId,
    b: ValueId,
    layout: crate::target::TargetLayout,
) -> AliasResult {
    // Same value → must alias.
    if a == b {
        return AliasResult::MustAlias;
    }

    // Trace both pointers to their base + offset.
    let base_a = trace_base(func, a, layout);
    let base_b = trace_base(func, b, layout);

    // Different base pointers → no alias (Fortran guarantee).
    match (&base_a, &base_b) {
        (PtrBase::Alloca(id_a), PtrBase::Alloca(id_b)) if id_a != id_b => {
            return AliasResult::NoAlias;
        }
        (PtrBase::Global(name_a), PtrBase::Global(name_b)) if name_a != name_b => {
            return AliasResult::NoAlias;
        }
        (PtrBase::Param(id_a), PtrBase::Param(id_b))
            if id_a != id_b
                && param_is_fortran_noalias(func, *id_a)
                && param_is_fortran_noalias(func, *id_b) =>
        {
            return AliasResult::NoAlias;
        }
        (PtrBase::Alloca(_), PtrBase::Global(_))
        | (PtrBase::Global(_), PtrBase::Alloca(_))
        | (PtrBase::Alloca(_), PtrBase::Param(_))
        | (PtrBase::Param(_), PtrBase::Alloca(_)) => {
            return AliasResult::NoAlias;
        }
        _ => {}
    }

    // Same base, different constant offsets → no alias.
    if base_a.base_id() == base_b.base_id() {
        let off_a = trace_offset(func, a, layout);
        let off_b = trace_offset(func, b, layout);
        if let (Some(oa), Some(ob)) = (off_a, off_b) {
            if oa != ob {
                if pointer_points_to_aggregate(func, a) || pointer_points_to_aggregate(func, b) {
                    return AliasResult::MayAlias;
                }
                return AliasResult::NoAlias;
            }
            // Same base + same offset → must alias.
            return AliasResult::MustAlias;
        }
    }

    AliasResult::MayAlias
}

fn pointer_points_to_aggregate(func: &Function, ptr: ValueId) -> bool {
    matches!(
        func.value_type(ptr),
        Some(IrType::Ptr(inner)) if matches!(inner.as_ref(), IrType::Array(..) | IrType::Struct(_))
    )
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
fn trace_base(func: &Function, ptr: ValueId, layout: crate::target::TargetLayout) -> PtrBase {
    // Check if this is a function parameter (pointer arg).
    for param in &func.params {
        if param.id == ptr {
            return PtrBase::Param(ptr);
        }
    }

    // Find the defining instruction.
    let Some(inst) = find_inst(func, ptr) else {
        return PtrBase::Unknown;
    };

    match &inst.kind {
        InstKind::Alloca(_) => PtrBase::Alloca(ptr),
        InstKind::GlobalAddr(name) => PtrBase::Global(name.clone()),
        InstKind::GetElementPtr(base, _) => trace_base(func, *base, layout),
        InstKind::Load(addr) => trace_param_wrapper(func, *addr, layout)
            .map(PtrBase::Param)
            .unwrap_or(PtrBase::Unknown),
        _ => PtrBase::Unknown,
    }
}

/// Trace a pointer to its constant byte offset from the base, if possible.
fn trace_offset(func: &Function, ptr: ValueId, layout: crate::target::TargetLayout) -> Option<i64> {
    if func.params.iter().any(|param| param.id == ptr) {
        return Some(0);
    }
    let inst = find_inst(func, ptr)?;
    match &inst.kind {
        InstKind::Alloca(_) | InstKind::GlobalAddr(_) => Some(0),
        InstKind::Load(addr) => trace_param_wrapper(func, *addr, layout).map(|_| 0),
        InstKind::GetElementPtr(base, indices) => {
            let base_offset = trace_offset(func, *base, layout)?;
            if indices.len() != 1 {
                return None;
            }
            let idx = resolve_const_int(func, indices[0])?;
            let step = match &inst.ty {
                IrType::Ptr(inner) => inner.size_bytes(&layout) as i64,
                _ => return None,
            };
            Some(base_offset + idx * step)
        }
        _ => None,
    }
}

fn param_is_fortran_noalias(func: &Function, param_id: ValueId) -> bool {
    func.params
        .iter()
        .find(|param| param.id == param_id)
        .map(|param| param.fortran_noalias)
        .unwrap_or(false)
}

fn trace_param_wrapper(
    func: &Function,
    addr: ValueId,
    layout: crate::target::TargetLayout,
) -> Option<ValueId> {
    let slot = match find_inst(func, addr).map(|inst| &inst.kind) {
        Some(InstKind::Alloca(_)) => addr,
        Some(InstKind::GetElementPtr(base, _)) if trace_offset(func, addr, layout)? == 0 => {
            match trace_base(func, *base, layout) {
                PtrBase::Alloca(slot) => slot,
                _ => return None,
            }
        }
        _ => return None,
    };

    let mut stored_param = None;
    for block in &func.blocks {
        for inst in &block.insts {
            let InstKind::Store(val, ptr) = &inst.kind else {
                continue;
            };
            if *ptr != slot {
                continue;
            }
            if !func.params.iter().any(|param| param.id == *val) {
                return None;
            }
            if stored_param.replace(*val).is_some() {
                return None;
            }
        }
    }

    stored_param
}

fn find_inst(func: &Function, vid: ValueId) -> Option<&Inst> {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.id == vid {
                return Some(inst);
            }
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
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    fn param(name: &str, id: u32, ty: IrType, fortran_noalias: bool) -> Param {
        Param {
            name: name.into(),
            ty,
            id: ValueId(id),
            fortran_noalias,
        }
    }

    #[test]
    fn distinct_allocas_no_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let a = f.next_value_id();
        f.register_type(a, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: a,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let b = f.next_value_id();
        f.register_type(b, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: b,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(&f, a, b, crate::target::TargetLayout::LP64),
            AliasResult::NoAlias
        );
    }

    #[test]
    fn same_pointer_must_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let a = f.next_value_id();
        f.register_type(a, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: a,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(&f, a, a, crate::target::TargetLayout::LP64),
            AliasResult::MustAlias
        );
    }

    #[test]
    fn mixed_width_geps_same_index_do_not_must_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let base_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384);
        let base = f.next_value_id();
        f.register_type(base, IrType::Ptr(Box::new(base_ty.clone())));
        f.block_mut(f.entry).insts.push(Inst {
            id: base,
            ty: IrType::Ptr(Box::new(base_ty)),
            span: span(),
            kind: InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384)),
        });

        let four = f.next_value_id();
        f.register_type(four, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: four,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(4, IntWidth::I64),
        });

        let gep_i32 = f.next_value_id();
        f.register_type(gep_i32, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep_i32,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GetElementPtr(base, vec![four]),
        });

        let gep_i64 = f.next_value_id();
        f.register_type(gep_i64, IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep_i64,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))),
            span: span(),
            kind: InstKind::GetElementPtr(base, vec![four]),
        });

        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(&f, gep_i32, gep_i64, crate::target::TargetLayout::LP64),
            AliasResult::NoAlias
        );
    }

    #[test]
    fn gep_different_offsets_no_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let base = f.next_value_id();
        f.register_type(base, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: base,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 10)),
        });
        // GEP at offset 0
        let c0 = f.next_value_id();
        f.register_type(c0, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c0,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(0, IntWidth::I64),
        });
        let gep0 = f.next_value_id();
        f.register_type(gep0, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep0,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GetElementPtr(base, vec![c0]),
        });
        // GEP at offset 1
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let gep1 = f.next_value_id();
        f.register_type(gep1, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep1,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GetElementPtr(base, vec![c1]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(&f, gep0, gep1, crate::target::TargetLayout::LP64),
            AliasResult::NoAlias
        );
    }

    #[test]
    fn aggregate_base_and_element_gep_may_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let base = f.next_value_id();
        f.register_type(
            base,
            IrType::Ptr(Box::new(IrType::Array(
                Box::new(IrType::Int(IntWidth::I32)),
                10,
            ))),
        );
        f.block_mut(f.entry).insts.push(Inst {
            id: base,
            ty: IrType::Ptr(Box::new(IrType::Array(
                Box::new(IrType::Int(IntWidth::I32)),
                10,
            ))),
            span: span(),
            kind: InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 10)),
        });

        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });

        let gep1 = f.next_value_id();
        f.register_type(gep1, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep1,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GetElementPtr(base, vec![c1]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(&f, base, gep1, crate::target::TargetLayout::LP64),
            AliasResult::MayAlias
        );
    }

    #[test]
    fn distinct_globals_no_alias() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let ga = f.next_value_id();
        f.register_type(ga, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: ga,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GlobalAddr("array_a".into()),
        });
        let gb = f.next_value_id();
        f.register_type(gb, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gb,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GlobalAddr("array_b".into()),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(&f, ga, gb, crate::target::TargetLayout::LP64),
            AliasResult::NoAlias
        );
    }

    #[test]
    fn distinct_noalias_params_do_not_alias() {
        let params = vec![
            param(
                "a",
                0,
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                true,
            ),
            param(
                "b",
                1,
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                true,
            ),
        ];
        let mut f = Function::new("test".into(), params, IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(
                &f,
                ValueId(0),
                ValueId(1),
                crate::target::TargetLayout::LP64
            ),
            AliasResult::NoAlias
        );
    }

    #[test]
    fn wrapper_loads_trace_back_to_noalias_params() {
        let params = vec![
            param(
                "a",
                0,
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                true,
            ),
            param(
                "b",
                1,
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                true,
            ),
        ];
        let mut f = Function::new("test".into(), params, IrType::Void);

        let slot_a = f.next_value_id();
        f.register_type(
            slot_a,
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        f.block_mut(f.entry).insts.push(Inst {
            id: slot_a,
            ty: IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
            span: span(),
            kind: InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
        });
        let slot_b = f.next_value_id();
        f.register_type(
            slot_b,
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        f.block_mut(f.entry).insts.push(Inst {
            id: slot_b,
            ty: IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
            span: span(),
            kind: InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
        });
        let store_a = f.next_value_id();
        f.register_type(store_a, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store_a,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(ValueId(0), slot_a),
        });
        let store_b = f.next_value_id();
        f.register_type(store_b, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store_b,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(ValueId(1), slot_b),
        });
        let load_a = f.next_value_id();
        f.register_type(load_a, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: load_a,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Load(slot_a),
        });
        let load_b = f.next_value_id();
        f.register_type(load_b, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: load_b,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Load(slot_b),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            query(&f, load_a, load_b, crate::target::TargetLayout::LP64),
            AliasResult::NoAlias
        );
    }
}
