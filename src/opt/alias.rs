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

use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::target::TargetLayout;
use std::collections::HashMap;

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

/// Hashable identity for a pointer location that is proven exact.
///
/// Equal keys imply `MustAlias`; unequal keys make no aliasing claim. Unknown
/// provenance is keyed by the original SSA value, preserving same-value
/// overwrites without conflating distinct dynamic pointers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ProvenLocation(ProvenLocationKind);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProvenLocationKind {
    Exact(PtrBase, i64),
    Opaque(ValueId),
}

/// Pointer provenance carried by a call argument.
///
/// `C_PTR` is represented as `i64` in the IR for ABI purposes, but a value
/// produced by `PtrToInt` still gives the callee access to the source
/// allocation. Call-side memory optimizations must classify that value like
/// the original pointer without treating unrelated integer arguments as
/// addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CallArgPointer {
    pub(crate) pointer: ValueId,
    pub(crate) may_carry_indirect_pointer: bool,
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
    layout: TargetLayout,
) -> bool {
    AliasOracle::new(func, layout).may_reach_through_call_arg(entry_ptr, call_arg)
}

/// Cached alias oracle for repeated queries within one function.
pub struct AliasOracle<'a> {
    func: &'a Function,
    layout: TargetLayout,
    insts: HashMap<ValueId, &'a Inst>,
    value_types: HashMap<ValueId, &'a IrType>,
    params: HashMap<ValueId, bool>,
    base_cache: HashMap<ValueId, PtrBase>,
    offset_cache: HashMap<ValueId, Option<i64>>,
    aggregate_cache: HashMap<ValueId, bool>,
    wrapper_cache: HashMap<ValueId, Option<ValueId>>,
    slot_param_cache: HashMap<ValueId, Option<ValueId>>,
    #[cfg(test)]
    query_count: usize,
}

impl<'a> AliasOracle<'a> {
    pub fn new(func: &'a Function, layout: TargetLayout) -> Self {
        let insts = func
            .blocks
            .iter()
            .flat_map(|block| block.insts.iter())
            .map(|inst| (inst.id, inst))
            .collect();
        let params = func
            .params
            .iter()
            .map(|param| (param.id, param.fortran_noalias))
            .collect();
        let value_types = func
            .params
            .iter()
            .map(|param| (param.id, &param.ty))
            .chain(
                func.blocks
                    .iter()
                    .flat_map(|block| block.params.iter().map(|param| (param.id, &param.ty))),
            )
            .chain(
                func.blocks
                    .iter()
                    .flat_map(|block| block.insts.iter().map(|inst| (inst.id, &inst.ty))),
            )
            .collect();
        Self {
            func,
            layout,
            insts,
            value_types,
            params,
            base_cache: HashMap::new(),
            offset_cache: HashMap::new(),
            aggregate_cache: HashMap::new(),
            wrapper_cache: HashMap::new(),
            slot_param_cache: HashMap::new(),
            #[cfg(test)]
            query_count: 0,
        }
    }

    /// Query whether two pointer values may alias.
    ///
    /// Both `a` and `b` should be pointer-typed values (results of Alloca,
    /// GlobalAddr, GetElementPtr, or function parameters).
    pub fn query(&mut self, a: ValueId, b: ValueId) -> AliasResult {
        #[cfg(test)]
        {
            self.query_count += 1;
        }

        // Same value → must alias.
        if a == b {
            return AliasResult::MustAlias;
        }

        // Trace both pointers to their base + offset.
        let base_a = self.trace_base(a);
        let base_b = self.trace_base(b);

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
                    && self.param_is_fortran_noalias(*id_a)
                    && self.param_is_fortran_noalias(*id_b) =>
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
        if base_a == base_b {
            let off_a = self.trace_offset(a);
            let off_b = self.trace_offset(b);
            if let (Some(oa), Some(ob)) = (off_a, off_b) {
                if oa != ob {
                    if self.pointer_points_to_aggregate(a) || self.pointer_points_to_aggregate(b) {
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

    /// Return a key suitable for indexing exact-location optimizations.
    pub(crate) fn proven_location(&mut self, ptr: ValueId) -> ProvenLocation {
        let base = self.trace_base(ptr);
        match (&base, self.trace_offset(ptr)) {
            (PtrBase::Alloca(_) | PtrBase::Global(_) | PtrBase::Param(_), Some(offset)) => {
                ProvenLocation(ProvenLocationKind::Exact(base, offset))
            }
            _ => ProvenLocation(ProvenLocationKind::Opaque(ptr)),
        }
    }

    #[cfg(test)]
    pub(crate) fn query_count(&self) -> usize {
        self.query_count
    }

    /// True if `entry_ptr` may be reachable through `call_arg` when
    /// a call passes `call_arg` across a function boundary.
    pub fn may_reach_through_call_arg(&mut self, entry_ptr: ValueId, call_arg: ValueId) -> bool {
        if entry_ptr == call_arg {
            return true;
        }
        let base_entry = self.trace_base(entry_ptr);
        let base_arg = self.trace_base(call_arg);
        match (&base_entry, &base_arg) {
            (PtrBase::Alloca(a), PtrBase::Alloca(b)) => a == b,
            (PtrBase::Global(a), PtrBase::Global(b)) => a == b,
            (PtrBase::Param(a), PtrBase::Param(b)) => {
                if a == b {
                    return true;
                }
                !(self.param_is_fortran_noalias(*a) && self.param_is_fortran_noalias(*b))
            }
            (PtrBase::Unknown, _) | (_, PtrBase::Unknown) => true,
            // Distinct kinds of allocations never alias per Fortran.
            _ => false,
        }
    }

    pub fn value_is_pointer(&self, value: ValueId) -> bool {
        matches!(self.value_type(value), Some(IrType::Ptr(_)))
    }

    /// Recover pointer provenance carried across a call boundary.
    ///
    /// Direct pointer operands and address-preserving `PtrToInt` results are
    /// recognized. Ordinary integers deliberately return `None`.
    pub(crate) fn call_arg_pointer(&self, value: ValueId) -> Option<CallArgPointer> {
        let pointer = if self.value_is_pointer(value) {
            value
        } else {
            match &self.find_inst(value)?.kind {
                InstKind::PtrToInt(pointer) if self.value_is_pointer(*pointer) => *pointer,
                _ => return None,
            }
        };
        let may_carry_indirect_pointer = matches!(
            self.value_type(pointer),
            Some(IrType::Ptr(inner))
                if matches!(
                    inner.as_ref(),
                    IrType::Array(..)
                        | IrType::Struct(_)
                        | IrType::Ptr(_)
                        | IrType::FuncPtr(_)
                )
        );
        Some(CallArgPointer {
            pointer,
            may_carry_indirect_pointer,
        })
    }

    pub(crate) fn value_type(&self, value: ValueId) -> Option<&'a IrType> {
        self.value_types.get(&value).copied()
    }

    /// Arbitrary calls can read or write globals without receiving their
    /// addresses as arguments. Unknown pointer provenance requires the same
    /// conservative barrier.
    pub fn requires_global_call_barrier(&mut self, ptr: ValueId) -> bool {
        matches!(self.trace_base(ptr), PtrBase::Global(_) | PtrBase::Unknown)
    }

    fn pointer_points_to_aggregate(&mut self, ptr: ValueId) -> bool {
        if let Some(points_to_aggregate) = self.aggregate_cache.get(&ptr) {
            return *points_to_aggregate;
        }

        let points_to_aggregate = matches!(
            self.value_type(ptr),
            Some(IrType::Ptr(inner)) if matches!(inner.as_ref(), IrType::Array(..) | IrType::Struct(_))
        );
        self.aggregate_cache.insert(ptr, points_to_aggregate);
        points_to_aggregate
    }

    /// Trace a pointer value back to its base allocation.
    fn trace_base(&mut self, ptr: ValueId) -> PtrBase {
        if let Some(base) = self.base_cache.get(&ptr) {
            return base.clone();
        }

        let base = self.compute_trace_base(ptr);
        self.base_cache.insert(ptr, base.clone());
        base
    }

    fn compute_trace_base(&mut self, ptr: ValueId) -> PtrBase {
        // Check if this is a function parameter (pointer arg).
        if self.params.contains_key(&ptr) {
            return PtrBase::Param(ptr);
        }

        // Find the defining instruction.
        let Some(kind) = self.find_inst(ptr).map(|inst| inst.kind.clone()) else {
            return PtrBase::Unknown;
        };

        match kind {
            InstKind::Alloca(_) => PtrBase::Alloca(ptr),
            InstKind::GlobalAddr(name) => PtrBase::Global(name),
            InstKind::GetElementPtr(base, _) => self.trace_base(base),
            InstKind::Load(addr) => self
                .trace_param_wrapper(addr)
                .map(PtrBase::Param)
                .unwrap_or(PtrBase::Unknown),
            _ => PtrBase::Unknown,
        }
    }

    /// Trace a pointer to its constant byte offset from the base, if possible.
    fn trace_offset(&mut self, ptr: ValueId) -> Option<i64> {
        if let Some(offset) = self.offset_cache.get(&ptr) {
            return *offset;
        }

        let offset = self.compute_trace_offset(ptr);
        self.offset_cache.insert(ptr, offset);
        offset
    }

    fn compute_trace_offset(&mut self, ptr: ValueId) -> Option<i64> {
        if self.params.contains_key(&ptr) {
            return Some(0);
        }
        let (kind, ty) = self
            .find_inst(ptr)
            .map(|inst| (inst.kind.clone(), inst.ty.clone()))?;
        match kind {
            InstKind::Alloca(_) | InstKind::GlobalAddr(_) => Some(0),
            InstKind::Load(addr) => self.trace_param_wrapper(addr).map(|_| 0),
            InstKind::GetElementPtr(base, indices) => {
                let base_offset = self.trace_offset(base)?;
                if indices.len() != 1 {
                    return None;
                }
                let idx = self.resolve_const_int(indices[0])?;
                let step = match &ty {
                    IrType::Ptr(inner) => i64::try_from(inner.size_bytes(&self.layout)).ok()?,
                    _ => return None,
                };
                base_offset.checked_add(idx.checked_mul(step)?)
            }
            _ => None,
        }
    }

    fn resolve_const_int(&self, value: ValueId) -> Option<i64> {
        match self.find_inst(value)?.kind {
            InstKind::ConstInt(constant, _) => i64::try_from(constant).ok(),
            _ => None,
        }
    }

    fn param_is_fortran_noalias(&self, param_id: ValueId) -> bool {
        self.params.get(&param_id).copied().unwrap_or(false)
    }

    fn trace_param_wrapper(&mut self, addr: ValueId) -> Option<ValueId> {
        if let Some(param) = self.wrapper_cache.get(&addr) {
            return *param;
        }

        let param = self.compute_trace_param_wrapper(addr);
        self.wrapper_cache.insert(addr, param);
        param
    }

    fn compute_trace_param_wrapper(&mut self, addr: ValueId) -> Option<ValueId> {
        let slot = match self.find_inst(addr).map(|inst| inst.kind.clone()) {
            Some(InstKind::Alloca(_)) => addr,
            Some(InstKind::GetElementPtr(base, _)) => {
                if self.trace_offset(addr)? != 0 {
                    return None;
                }
                match self.trace_base(base) {
                    PtrBase::Alloca(slot) => slot,
                    _ => return None,
                }
            }
            _ => return None,
        };

        self.stored_param_for_slot(slot)
    }

    fn stored_param_for_slot(&mut self, slot: ValueId) -> Option<ValueId> {
        if let Some(param) = self.slot_param_cache.get(&slot) {
            return *param;
        }

        let result = 'scan: {
            let mut stored_param = None;
            for block in &self.func.blocks {
                for inst in &block.insts {
                    let InstKind::Store(val, ptr) = &inst.kind else {
                        continue;
                    };
                    if *ptr != slot {
                        continue;
                    }
                    if !self.params.contains_key(val) {
                        break 'scan None;
                    }
                    if stored_param.replace(*val).is_some() {
                        break 'scan None;
                    }
                }
            }
            stored_param
        };

        self.slot_param_cache.insert(slot, result);
        result
    }

    fn find_inst(&self, vid: ValueId) -> Option<&'a Inst> {
        self.insts.get(&vid).copied()
    }
}

/// Query whether two pointer values may alias.
///
/// Both `a` and `b` should be pointer-typed values (results of Alloca,
/// GlobalAddr, GetElementPtr, or function parameters).
pub fn query(func: &Function, a: ValueId, b: ValueId, layout: TargetLayout) -> AliasResult {
    AliasOracle::new(func, layout).query(a, b)
}

/// Traced pointer base — the root allocation or global.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PtrBase {
    Alloca(ValueId),
    Global(String),
    Param(ValueId),
    Unknown,
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
        let base_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
        let base = f.next_value_id();
        f.register_type(base, IrType::Ptr(Box::new(base_ty.clone())));
        f.block_mut(f.entry).insts.push(Inst {
            id: base,
            ty: IrType::Ptr(Box::new(base_ty)),
            span: span(),
            kind: InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392)),
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

    #[test]
    fn ptr_to_int_call_arg_retains_pointer_provenance() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let pointer = f.next_value_id();
        f.register_type(pointer, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: pointer,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let c_ptr = f.next_value_id();
        f.register_type(c_ptr, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c_ptr,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::PtrToInt(pointer),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            AliasOracle::new(&f, crate::target::TargetLayout::LP64).call_arg_pointer(c_ptr),
            Some(CallArgPointer {
                pointer,
                may_carry_indirect_pointer: false,
            })
        );
    }

    #[test]
    fn ordinary_i64_call_arg_has_no_pointer_provenance() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let integer = f.next_value_id();
        f.register_type(integer, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: integer,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(42, IntWidth::I64),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));

        assert_eq!(
            AliasOracle::new(&f, crate::target::TargetLayout::LP64).call_arg_pointer(integer),
            None
        );
    }
}
