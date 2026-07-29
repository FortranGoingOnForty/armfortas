//! Allocation of local variables from declarations.
//!
//! Extracted from `core.rs` in Sprint 11 Stage E. Pure mechanical
//! move — behavior unchanged.

use std::collections::HashMap;

use crate::ast::decl::{ArraySpec, Decl, TypeSpec};
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;
use crate::sema::symtab::SymbolTable;

use super::const_scalar::{clamp_const_to_type, ConstScalar};
use super::core::*;
use super::ctx::{current_proc_scope, CharKind, LocalInfo};
use super::helpers::{clamp_nonnegative_i64, widen_to_i64};

fn canonical_declared_derived_layout_name(
    type_spec: &TypeSpec,
    st: &SymbolTable,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<String> {
    match type_spec {
        TypeSpec::Type(type_name) | TypeSpec::Class(type_name) => {
            canonical_layout_type_name_for_scope(st, current_proc_scope(), type_name, type_layouts)
        }
        _ => None,
    }
}

fn declared_derived_info_name(
    type_spec: &TypeSpec,
    st: &SymbolTable,
    canonical_layout_name: Option<&str>,
) -> Option<String> {
    match type_spec {
        TypeSpec::Type(type_name) | TypeSpec::Class(type_name) => {
            if st
                .find_symbol_any_scope(&type_name.to_lowercase())
                .is_some_and(|sym| {
                    matches!(sym.kind, crate::sema::symtab::SymbolKind::EnumerationType)
                })
            {
                None
            } else {
                Some(
                    canonical_layout_name
                        .map(str::to_owned)
                        .unwrap_or_else(|| type_name.clone()),
                )
            }
        }
        _ => None,
    }
}

/// Allocate local variables from declarations. Handles both scalars and arrays.
pub(crate) fn alloc_decls(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, LocalInfo>,
    decls: &[crate::ast::decl::SpannedDecl],
    visible_param_consts: &HashMap<String, ConstScalar>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    pending_globals: &mut Vec<PendingGlobal>,
    func_name: &str,
    st: &SymbolTable,
) {
    use crate::ast::decl::Attribute;

    // Pre-scan standalone PARAMETER statements so a TypeDecl entity
    // whose value comes from a separate `parameter (name = expr)`
    // statement still triggers SAVE-promotion at alloc time. Without
    // this pre-scan, the standalone form would silently fall back to
    // the alloca + per-call store path.
    let mut parameter_inits: HashMap<String, &crate::ast::expr::SpannedExpr> = HashMap::new();
    for d in decls {
        if let Decl::ParameterStmt { pairs } = &d.node {
            for (name, expr) in pairs {
                parameter_inits.insert(name.to_lowercase(), expr);
            }
        }
    }

    // Audit CRITICAL-1: build the per-scope parameter constants
    // table so SAVE-promotion's eval_const_global_init can resolve
    // `Expr::Name` references against compile-time-known parameters
    // declared earlier in the same scope. Without this, an init
    // like `integer :: x = k * 2` (k a parameter) silently falls
    // back to alloca + per-call store and breaks SAVE semantics.
    //
    // Parameters can reference earlier parameters (`tau = 2 * pi`),
    // so we walk decls in order and build the map incrementally.
    let param_consts = collect_decl_param_consts_with_scope(decls, visible_param_consts, st);
    let param_char_consts = collect_decl_param_char_consts(
        decls,
        &param_consts,
        type_layouts,
        st,
        current_proc_scope(),
    );

    for decl in decls {
        if let Decl::TypeDecl {
            type_spec,
            attrs,
            entities,
        } = &decl.node
        {
            let elem_ty =
                lower_type_spec_with_param_consts(type_spec, Some(&param_consts), Some(st));
            let declared_derived_layout_name =
                canonical_declared_derived_layout_name(type_spec, st, type_layouts);
            let declared_derived_info_name =
                declared_derived_info_name(type_spec, st, declared_derived_layout_name.as_deref());

            let attr_dims: Option<&Vec<ArraySpec>> = attrs.iter().find_map(|a| {
                if let Attribute::Dimension(specs) = a {
                    Some(specs)
                } else {
                    None
                }
            });
            let is_allocatable = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
            let is_pointer_attr = attrs.iter().any(|a| matches!(a, Attribute::Pointer));

            for entity in entities {
                let key = entity.name.to_lowercase();
                if locals.contains_key(&key) {
                    continue;
                }
                let init_expr: Option<&crate::ast::expr::SpannedExpr> = entity
                    .init
                    .as_ref()
                    .or_else(|| parameter_inits.get(&key).copied());
                let is_parameter = attrs.iter().any(|a| matches!(a, Attribute::Parameter))
                    || parameter_inits.contains_key(&key);

                // Use entity-level array spec, or fall back to attribute-level DIMENSION.
                let array_spec = entity.array_spec.as_ref().or(attr_dims);

                // Check for character type.
                let char_len = declared_char_len(
                    type_spec,
                    entity.char_len.as_ref(),
                    init_expr,
                    &param_consts,
                    &param_char_consts,
                    st,
                    Some(type_layouts),
                    current_proc_scope(),
                );
                let effective_char_len =
                    super::core::effective_decl_char_len_spec(type_spec, entity.char_len.as_ref());
                let runtime_char_len_expr = match effective_char_len {
                    Some(crate::ast::decl::LenSpec::Expr(e))
                        if eval_const_int_in_scope_or_any_scope(e, &param_consts, st).is_none() =>
                    {
                        Some(e)
                    }
                    _ => None,
                };
                let is_deferred_char =
                    matches!(effective_char_len, Some(crate::ast::decl::LenSpec::Colon));

                if is_pointer_attr && array_spec.is_some() {
                    // Pointer to array.  Reuses the 392-byte array
                    // descriptor layout that allocatables use: the
                    // pointer slot carries base_addr, elem_size,
                    // rank, flags, and per-dim bounds so that
                    // downstream subscript / SIZE / whole-array
                    // operations pick it up through the existing
                    // descriptor path.  `=>` fills the slot from a
                    // materialised descriptor of the target (see
                    // Stmt::PointerAssignment).  Unassociated state
                    // is encoded by flags=0, same as an unallocated
                    // allocatable.
                    //
                    // We set `allocatable = true` so that
                    // `local_uses_array_descriptor` and
                    // `array_descriptor_addr` treat the slot as a
                    // descriptor-at-info.addr (no extra indirection).
                    // `is_pointer = true` is separately used by
                    // scope-exit deallocation to suppress the
                    // afs_deallocate_array call — a pointer does
                    // not own its target.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                    let addr = b.alloca(desc_ty);
                    let zero_byte = b.const_i32(0);
                    let descriptor_bytes = b.const_i64(392);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![addr, zero_byte, descriptor_bytes],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    // dims is left empty for a deferred-shape pointer;
                    // the descriptor carries the runtime rank and
                    // bounds after `=>` binds it to a target.
                    let pointer_elem_ty = if matches!(type_spec, TypeSpec::Character(_)) {
                        match char_len {
                            Some(len) => fixed_char_storage_ir_type(len),
                            None => elem_ty.clone(),
                        }
                    } else if let Some(type_name) = declared_derived_layout_name.as_deref() {
                        derived_storage_ir_type(type_name, type_layouts)
                            .unwrap_or_else(|| elem_ty.clone())
                    } else {
                        elem_ty.clone()
                    };
                    let pointer_char_kind = if matches!(type_spec, TypeSpec::Character(_)) {
                        match char_len {
                            Some(len) => CharKind::Fixed(len),
                            None => CharKind::None,
                        }
                    } else {
                        CharKind::None
                    };
                    locals.insert(
                        key,
                        LocalInfo {
                            addr,
                            ty: pointer_elem_ty,
                            dims: vec![],
                            allocatable: true,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: pointer_char_kind,
                            derived_type: declared_derived_info_name.clone(),
                            inline_const: None,
                            is_pointer: true,
                            runtime_dim_upper: array_spec
                                .as_ref()
                                .map(|specs| vec![None; specs.len()])
                                .unwrap_or_default(),
                            is_class: matches!(type_spec, TypeSpec::Class(_) | TypeSpec::ClassStar),
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                    continue;
                }
                if is_pointer_attr && matches!(type_spec, TypeSpec::Type(_)) && array_spec.is_none()
                {
                    // Pointer to derived type.  Slot holds an 8-byte
                    // pointer to the target struct; ComponentAccess
                    // loads the slot and uses that address as the
                    // struct base.  derived_type is stored so that
                    // component lookup can find the type layout.
                    if let TypeSpec::Type(_) = type_spec {
                        let addr = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        let zero_byte = b.const_i32(0);
                        let eight = b.const_i64(8);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![addr, zero_byte, eight],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::None,
                                derived_type: declared_derived_info_name.clone(),
                                inline_const: None,
                                is_pointer: true,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                        continue;
                    }
                }
                if is_pointer_attr
                    && matches!(type_spec, TypeSpec::Class(_) | TypeSpec::ClassStar)
                    && array_spec.is_none()
                {
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                    let addr = b.alloca(desc_ty);
                    let zero = b.const_i32(0);
                    let descriptor_bytes = b.const_i64(392);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![addr, zero, descriptor_bytes],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    locals.insert(
                        key,
                        LocalInfo {
                            addr,
                            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            dims: vec![],
                            allocatable: false,
                            descriptor_arg: true,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: declared_derived_info_name.clone(),
                            inline_const: None,
                            is_pointer: true,
                            runtime_dim_upper: vec![],
                            is_class: true,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                    continue;
                }
                if is_deferred_char && (is_allocatable || is_pointer_attr) && array_spec.is_none() {
                    // Deferred-length allocatable/pointer scalar character:
                    // 32-byte StringDescriptor. Deferred-length arrays fall
                    // through to the general descriptor path below.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 32);
                    let addr = b.alloca(desc_ty);
                    let zero = b.const_i32(0);
                    let size32 = b.const_i64(32);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![addr, zero, size32],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    locals.insert(
                        key,
                        LocalInfo {
                            addr,
                            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            dims: vec![],
                            allocatable: true,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::Deferred,
                            derived_type: None,
                            inline_const: None,
                            is_pointer: is_pointer_attr,
                            runtime_dim_upper: vec![],
                            is_class: false,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                    continue;
                }

                if let Some(specs) = array_spec.filter(|_| !is_allocatable) {
                    if (!matches!(type_spec, TypeSpec::Character(_)) || char_len.is_some())
                        && array_spec_has_runtime_bounds(specs, &param_consts, Some(st))
                    {
                        let dims =
                            extract_array_dims_with_init(specs, init_expr, &param_consts, Some(st));
                        let (array_elem_ty, array_derived_type, array_char_kind) = if matches!(
                            type_spec,
                            TypeSpec::Character(_)
                        ) {
                            let len = char_len.expect(
                                    "runtime-bound explicit-shape character array should have a fixed element length",
                            );
                            (fixed_char_storage_ir_type(len), None, CharKind::Fixed(len))
                        } else if let Some(type_name) = declared_derived_layout_name.as_ref() {
                            if let Some(layout) = type_layouts.get(type_name) {
                                (
                                    IrType::Array(
                                        Box::new(IrType::Int(IntWidth::I8)),
                                        layout.size as u64,
                                    ),
                                    Some(type_name.clone()),
                                    CharKind::None,
                                )
                            } else {
                                (elem_ty.clone(), None, CharKind::None)
                            }
                        } else {
                            (elem_ty.clone(), None, CharKind::None)
                        };

                        let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                        let addr = b.alloca(desc_ty);
                        let zero = b.const_i32(0);
                        let descriptor_bytes = b.const_i64(392);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![addr, zero, descriptor_bytes],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );

                        let rank = specs.len();
                        let one_i64 = b.const_i64(1);
                        let dim_buf = if rank == 0 {
                            b.const_i64(0)
                        } else {
                            let dim_buf_bytes = (rank * 24) as u64;
                            let dim_buf = b.alloca(IrType::Array(
                                Box::new(IrType::Int(IntWidth::I8)),
                                dim_buf_bytes,
                            ));
                            // Column-major stride accumulator: dim[k].stride =
                            // product(extents[0..k]). Without this, every dim
                            // got stride=1 — `center(i, :) = ...` walked the
                            // row axis in element-stride-1 (touching only the
                            // first column entry per row) instead of the
                            // column axis in stride=m, so multi-dim section
                            // assigns to a runtime-shape local silently
                            // wrote bogus values.
                            let mut running_stride = one_i64;
                            for (i, spec) in specs.iter().enumerate() {
                                let (lo64, up64) = match spec {
                                    ArraySpec::Explicit { lower, upper } => {
                                        let lo64 = lower
                                            .as_ref()
                                            .and_then(|expr| {
                                                eval_const_array_bound(
                                                    expr,
                                                    &param_consts,
                                                    Some(st),
                                                )
                                            })
                                            .map(|value| b.const_i64(value))
                                            .unwrap_or_else(|| {
                                                if let Some(expr) = lower.as_ref() {
                                                    let raw = super::expr::lower_expr_with_optional_layouts(
                                                        b,
                                                        locals,
                                                        expr,
                                                        st,
                                                        Some(type_layouts),
                                                    );
                                                    widen_to_i64(b, raw)
                                                } else {
                                                    b.const_i64(1)
                                                }
                                            });
                                        let up64 = eval_const_array_bound(
                                            upper,
                                            &param_consts,
                                            Some(st),
                                        )
                                        .map(|value| b.const_i64(value))
                                        .unwrap_or_else(|| {
                                            let raw = super::expr::lower_expr_with_optional_layouts(
                                                b,
                                                locals,
                                                upper,
                                                st,
                                                Some(type_layouts),
                                            );
                                            widen_to_i64(b, raw)
                                        });
                                        (lo64, up64)
                                    }
                                    _ => (b.const_i64(1), b.const_i64(1)),
                                };
                                let base = (i * 24) as i64;
                                let off_lo = b.const_i64(base);
                                let off_up = b.const_i64(base + 8);
                                let off_st = b.const_i64(base + 16);
                                let p_lo = b.gep(dim_buf, vec![off_lo], IrType::Int(IntWidth::I8));
                                let p_up = b.gep(dim_buf, vec![off_up], IrType::Int(IntWidth::I8));
                                let p_st = b.gep(dim_buf, vec![off_st], IrType::Int(IntWidth::I8));
                                b.store(lo64, p_lo);
                                b.store(up64, p_up);
                                b.store(running_stride, p_st);
                                if i + 1 < rank {
                                    let span = b.isub(up64, lo64);
                                    let extent = b.iadd(span, one_i64);
                                    running_stride = b.imul(running_stride, extent);
                                }
                            }
                            dim_buf
                        };

                        let elem_size = b.const_i64(ir_scalar_byte_size(&array_elem_ty, b.layout));
                        let rank_val = b.const_i32(rank as i32);
                        let stat_slot = b.alloca(IrType::Int(IntWidth::I32));
                        b.call(
                            FuncRef::External("afs_allocate_array".into()),
                            vec![addr, elem_size, rank_val, dim_buf, stat_slot],
                            IrType::Void,
                        );

                        if let Some(len) = char_len {
                            let total = b.call(
                                FuncRef::External("afs_array_size".into()),
                                vec![addr],
                                IrType::Int(IntWidth::I64),
                            );
                            let byte_count = if len == 1 {
                                total
                            } else {
                                let elem_len = b.const_i64(len);
                                b.imul(total, elem_len)
                            };
                            let byte_base = if matches!(array_elem_ty, IrType::Int(IntWidth::I8)) {
                                b.load_typed(addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                            } else {
                                let base = b
                                    .load_typed(addr, IrType::Ptr(Box::new(array_elem_ty.clone())));
                                let zero = b.const_i64(0);
                                b.gep(base, vec![zero], IrType::Int(IntWidth::I8))
                            };
                            let space = b.const_i32(b' ' as i32);
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![byte_base, space, byte_count],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                        }

                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: array_elem_ty,
                                dims,
                                allocatable: true,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: array_char_kind,
                                derived_type: array_derived_type,
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                        continue;
                    }
                }

                if let Some(len) = char_len {
                    if let Some(specs) = array_spec.filter(|_| !is_allocatable) {
                        // Fixed-length character arrays use contiguous inline
                        // element storage, not a pointer-slot table. The slot
                        // table was a legacy lowering artifact that let reads
                        // load the element bytes as if they were an address,
                        // which is exactly how local `character(len=N),
                        // parameter :: builtins(...)` ended up crashing fortsh
                        // `type`/`command` with `0x202020...` memmove faults.
                        let dims =
                            extract_array_dims_with_init(specs, init_expr, &param_consts, Some(st));
                        let total_size: i64 = dims.iter().map(|(_, size)| *size).product();
                        let elem_ty = fixed_char_storage_ir_type(len);
                        let elem_bytes = ir_scalar_byte_size(&elem_ty, b.layout);
                        let total_bytes = total_size * elem_bytes;
                        let space = b.const_i32(b' ' as i32);
                        let total_bytes_val = b.const_i64(total_bytes);
                        const STACK_THRESHOLD: i64 = 64 * 1024;

                        if total_bytes >= STACK_THRESHOLD {
                            let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                            let addr = b.alloca(desc_ty);
                            let zero = b.const_i32(0);
                            let descriptor_bytes = b.const_i64(392);
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![addr, zero, descriptor_bytes],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            let es = b.const_i64(elem_bytes);
                            let n = b.const_i64(total_size);
                            b.call(
                                FuncRef::External("afs_allocate_1d".into()),
                                vec![addr, es, n],
                                IrType::Void,
                            );
                            rewrite_heap_promoted_declared_bounds(b, addr, &dims);
                            let base = b
                                .load_typed(addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![base, space, total_bytes_val],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            locals.insert(
                                key,
                                LocalInfo {
                                    addr,
                                    ty: elem_ty,
                                    dims,
                                    allocatable: true,
                                    descriptor_arg: false,
                                    by_ref: false,
                                    char_kind: CharKind::Fixed(len),
                                    derived_type: None,
                                    inline_const: None,
                                    is_pointer: false,
                                    runtime_dim_upper: vec![],
                                    is_class: false,
                                    logical_kind: None,
                                    last_dim_assumed_size: false,
                                },
                            );
                        } else {
                            let arr_ty =
                                IrType::Array(Box::new(elem_ty.clone()), total_size as u64);
                            let addr = b.alloca(arr_ty);
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![addr, space, total_bytes_val],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            locals.insert(
                                key,
                                LocalInfo {
                                    addr,
                                    ty: elem_ty,
                                    dims,
                                    allocatable: false,
                                    descriptor_arg: false,
                                    by_ref: false,
                                    char_kind: CharKind::Fixed(len),
                                    derived_type: None,
                                    inline_const: None,
                                    is_pointer: false,
                                    runtime_dim_upper: vec![],
                                    is_class: false,
                                    logical_kind: None,
                                    last_dim_assumed_size: false,
                                },
                            );
                        }
                        continue;
                    }
                    if is_pointer_attr && array_spec.is_none() {
                        // Fixed-length scalar character POINTERs use a pointer
                        // slot, not inline character storage. Intrinsics like
                        // c_f_pointer populate this slot with the associated
                        // byte buffer address, and later substring/character
                        // reads must dereference it.
                        let addr = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        let zero = b.const_i32(0);
                        let eight = b.const_i64(8);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![addr, zero, eight],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: IrType::Int(IntWidth::I8),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::Fixed(len),
                                derived_type: None,
                                inline_const: None,
                                is_pointer: true,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                        continue;
                    }
                    if !is_allocatable {
                        // Fixed-length character(N): alloca N+1 bytes so call-boundary
                        // lowering can rely on a stable trailing NUL while the Fortran
                        // value still occupies the first N bytes.
                        let buf_ty =
                            IrType::Array(Box::new(IrType::Int(IntWidth::I8)), (len + 1) as u64);
                        let is_save_attr = attrs.iter().any(|a| matches!(a, Attribute::Save));
                        if !is_parameter
                            && array_spec.is_none()
                            && (is_save_attr || init_expr.is_some())
                        {
                            let mut bytes = vec![b' '; len.max(0) as usize + 1];
                            let mut const_init = init_expr.is_none();
                            if let Some(expr) = init_expr {
                                if let Some(raw) =
                                    eval_const_char_bytes(expr, &param_consts, &param_char_consts)
                                {
                                    const_init = true;
                                    let limit = len.max(0) as usize;
                                    for (dst, src) in
                                        bytes.iter_mut().take(limit).zip(raw.iter().copied())
                                    {
                                        *dst = src;
                                    }
                                }
                            }
                            if const_init {
                                let global_name = save_global_name(func_name, &key);
                                pending_globals.push(PendingGlobal {
                                    global: Global {
                                        name: global_name.clone(),
                                        ty: buf_ty.clone(),
                                        initializer: Some(GlobalInit::String(bytes)),
                                    },
                                });
                                let addr = b.global_addr(&global_name, buf_ty);
                                locals.insert(
                                    key,
                                    LocalInfo {
                                        addr,
                                        ty: IrType::Int(IntWidth::I8),
                                        dims: vec![],
                                        allocatable: false,
                                        descriptor_arg: false,
                                        by_ref: false,
                                        char_kind: CharKind::Fixed(len),
                                        derived_type: None,
                                        inline_const: None,
                                        is_pointer: false,
                                        runtime_dim_upper: vec![],
                                        is_class: false,
                                        logical_kind: None,
                                        last_dim_assumed_size: false,
                                    },
                                );
                                continue;
                            }
                        }
                        let addr = b.alloca(buf_ty);
                        let zero = b.const_i32(0);
                        let total = b.const_i64(len + 1);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![addr, zero, total],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        // Initialize with spaces.
                        let space = b.const_i32(b' ' as i32);
                        let len_val = b.const_i64(len);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![addr, space, len_val],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: IrType::Int(IntWidth::I8),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::Fixed(len),
                                derived_type: None,
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                        continue; // skip normal path
                    }
                } else if let Some(len_expr) = runtime_char_len_expr {
                    if !is_allocatable && array_spec.is_none() {
                        // Automatic fixed-length character whose size depends on a
                        // runtime expression such as LEN(input). Pointer
                        // declarations keep only a pointer slot plus the
                        // runtime length; intrinsics such as c_f_pointer bind
                        // the slot to external storage that this local does
                        // not own.
                        let raw_len = super::expr::lower_expr_with_optional_layouts(
                            b,
                            locals,
                            len_expr,
                            st,
                            Some(type_layouts),
                        );
                        let len_val = clamp_nonnegative_i64(b, raw_len);
                        let len_addr = b.alloca(IrType::Int(IntWidth::I64));
                        b.store(len_val, len_addr);

                        let ptr_slot = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        let zero = b.const_i32(0);
                        let eight = b.const_i64(8);
                        if is_pointer_attr {
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![ptr_slot, zero, eight],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                        } else {
                            let one = b.const_i64(1);
                            let total = b.iadd(len_val, one);
                            let ptr = b.runtime_call(
                                RuntimeFunc::Allocate,
                                vec![total],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            b.store(ptr, ptr_slot);

                            b.call(
                                FuncRef::External("memset".into()),
                                vec![ptr, zero, total],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            let space = b.const_i32(b' ' as i32);
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![ptr, space, len_val],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                        }

                        locals.insert(
                            key,
                            LocalInfo {
                                addr: ptr_slot,
                                ty: IrType::Int(IntWidth::I8),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::FixedRuntime { len_addr },
                                derived_type: None,
                                inline_const: None,
                                is_pointer: is_pointer_attr,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                        continue;
                    }
                }

                if is_allocatable {
                    // Allocatable variable: alloca a descriptor (392 bytes), zero-initialized.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                    let addr = b.alloca(desc_ty);
                    // Zero-initialize the descriptor so flags=0 (not allocated).
                    let zero = b.const_i32(0);
                    let size = b.const_i64(392);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![addr, zero, size],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    let alloc_elem_ty = if matches!(type_spec, TypeSpec::Character(_)) {
                        match char_len {
                            Some(len) => fixed_char_storage_ir_type(len),
                            None => elem_ty.clone(),
                        }
                    } else if let Some(type_name) = declared_derived_layout_name.as_deref() {
                        derived_storage_ir_type(type_name, type_layouts)
                            .unwrap_or_else(|| elem_ty.clone())
                    } else {
                        elem_ty.clone()
                    };
                    let char_kind = match char_len {
                        Some(len) if array_spec.is_some() => CharKind::Fixed(len),
                        _ => CharKind::None,
                    };
                    locals.insert(
                        key,
                        LocalInfo {
                            addr,
                            ty: alloc_elem_ty,
                            dims: vec![],
                            allocatable: true,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind,
                            derived_type: declared_derived_info_name.clone(),
                            inline_const: None,
                            is_pointer: false,
                            runtime_dim_upper: array_spec
                                .as_ref()
                                .map(|specs| vec![None; specs.len()])
                                .unwrap_or_default(),
                            is_class: matches!(type_spec, TypeSpec::Class(_) | TypeSpec::ClassStar),
                            logical_kind: if let TypeSpec::Logical(sel) = type_spec {
                                Some(extract_kind_with_context(
                                    sel,
                                    4,
                                    Some(&param_consts),
                                    Some(st),
                                ))
                            } else {
                                None
                            },
                            last_dim_assumed_size: false,
                        },
                    );
                } else if let Some(specs) = array_spec {
                    // Fixed-size array variable.
                    let dims =
                        extract_array_dims_with_init(specs, init_expr, &param_consts, Some(st));
                    let total_size: i64 = dims.iter().map(|(_, size)| *size).product();
                    if matches!(type_spec, TypeSpec::Character(_)) && char_len.is_none() {
                        if let Some(len_expr) = runtime_char_len_expr {
                            let raw_len = super::expr::lower_expr_with_optional_layouts(
                                b,
                                locals,
                                len_expr,
                                st,
                                Some(type_layouts),
                            );
                            let len_val = clamp_nonnegative_i64(b, raw_len);
                            let len_addr = b.alloca(IrType::Int(IntWidth::I64));
                            b.store(len_val, len_addr);

                            let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                            let addr = b.alloca(desc_ty);
                            let zero = b.const_i32(0);
                            let descriptor_bytes = b.const_i64(392);
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![addr, zero, descriptor_bytes],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );

                            let n = b.const_i64(total_size.max(0));
                            b.call(
                                FuncRef::External("afs_allocate_1d".into()),
                                vec![addr, len_val, n],
                                IrType::Void,
                            );
                            rewrite_heap_promoted_declared_bounds(b, addr, &dims);

                            let base = b
                                .load_typed(addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                            let total_bytes = b.imul(len_val, n);
                            let space = b.const_i32(b' ' as i32);
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![base, space, total_bytes],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );

                            locals.insert(
                                key,
                                LocalInfo {
                                    addr,
                                    ty: IrType::Int(IntWidth::I8),
                                    dims,
                                    allocatable: true,
                                    descriptor_arg: false,
                                    by_ref: false,
                                    char_kind: CharKind::FixedRuntime { len_addr },
                                    derived_type: None,
                                    inline_const: None,
                                    is_pointer: false,
                                    runtime_dim_upper: vec![],
                                    is_class: false,
                                    logical_kind: None,
                                    last_dim_assumed_size: false,
                                },
                            );
                            continue;
                        }
                    }
                    let (array_elem_ty, array_derived_type, array_char_kind) =
                        if matches!(type_spec, TypeSpec::Character(_)) {
                            if let Some(len) = char_len {
                                (fixed_char_storage_ir_type(len), None, CharKind::Fixed(len))
                            } else {
                                (elem_ty.clone(), None, CharKind::None)
                            }
                        } else if let Some(type_name) = declared_derived_layout_name.as_ref() {
                            if let Some(layout) = type_layouts.get(type_name) {
                                (
                                    IrType::Array(
                                        Box::new(IrType::Int(IntWidth::I8)),
                                        layout.size as u64,
                                    ),
                                    Some(type_name.clone()),
                                    CharKind::None,
                                )
                            } else {
                                (elem_ty.clone(), None, CharKind::None)
                            }
                        } else {
                            (elem_ty.clone(), None, CharKind::None)
                        };
                    let elem_bytes = ir_scalar_byte_size(&array_elem_ty, b.layout);
                    let total_bytes = total_size * elem_bytes;
                    const STACK_THRESHOLD: i64 = 64 * 1024; // 64KB

                    if total_bytes >= STACK_THRESHOLD {
                        // Large array: use descriptor + heap allocation (prevents stack overflow).
                        let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                        let addr = b.alloca(desc_ty);
                        let zero = b.const_i32(0);
                        let descriptor_bytes = b.const_i64(392);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![addr, zero, descriptor_bytes],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        // Auto-allocate with the declared shape.
                        let es = b.const_i64(elem_bytes);
                        let n = b.const_i64(total_size);
                        b.call(
                            FuncRef::External("afs_allocate_1d".into()),
                            vec![addr, es, n],
                            IrType::Void,
                        );
                        rewrite_heap_promoted_declared_bounds(b, addr, &dims);
                        if let Some(ref type_name) = array_derived_type {
                            if let Some(layout) = type_layouts.get(type_name) {
                                if derived_layout_needs_runtime_initialization(layout, type_layouts)
                                {
                                    let base_ptr = b.load_typed(
                                        addr,
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                    initialize_derived_array_storage(
                                        b,
                                        base_ptr,
                                        layout,
                                        total_size.max(0),
                                        type_layouts,
                                    );
                                }
                            }
                        }
                        // Mark as allocatable so scope-exit dealloc fires.
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: array_elem_ty.clone(),
                                dims,
                                allocatable: true,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: array_char_kind.clone(),
                                derived_type: array_derived_type.clone(),
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: if let TypeSpec::Logical(sel) = type_spec {
                                    Some(extract_kind_with_context(
                                        sel,
                                        4,
                                        Some(&param_consts),
                                        Some(st),
                                    ))
                                } else {
                                    None
                                },
                                last_dim_assumed_size: false,
                            },
                        );
                    } else {
                        // Small array: stack allocation.
                        let arr_ty =
                            IrType::Array(Box::new(array_elem_ty.clone()), total_size as u64);
                        let addr = b.alloca(arr_ty);
                        if let Some(ref type_name) = array_derived_type {
                            if let Some(layout) = type_layouts.get(type_name) {
                                if derived_layout_needs_runtime_initialization(layout, type_layouts)
                                {
                                    initialize_derived_array_storage(
                                        b,
                                        addr,
                                        layout,
                                        total_size.max(0),
                                        type_layouts,
                                    );
                                }
                            }
                        }
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: array_elem_ty.clone(),
                                dims,
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: array_char_kind,
                                derived_type: array_derived_type,
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: type_spec_logical_kind(
                                    type_spec,
                                    Some(&param_consts),
                                    Some(st),
                                ),
                                last_dim_assumed_size: false,
                            },
                        );
                    }
                } else if let TypeSpec::Type(ref type_name) = type_spec {
                    // TYPE(name) also spells F2023 enumeration and
                    // named-enum types — scalar integer ordinals, not
                    // struct storage (7.6.2 NOTE uses Type(v_value)).
                    if let Some(sym) = st.find_symbol_any_scope(&type_name.to_lowercase()) {
                        if matches!(sym.kind, crate::sema::symtab::SymbolKind::EnumerationType) {
                            let scalar_ty = sym
                                .type_info
                                .as_ref()
                                .map(crate::ir::lower::core::type_info_to_ir_type)
                                .unwrap_or(IrType::Int(IntWidth::I32));
                            let addr = b.alloca(scalar_ty.clone());
                            locals.insert(
                                key,
                                LocalInfo {
                                    addr,
                                    ty: scalar_ty,
                                    dims: vec![],
                                    allocatable: false,
                                    descriptor_arg: false,
                                    by_ref: false,
                                    char_kind: CharKind::None,
                                    derived_type: None,
                                    inline_const: None,
                                    is_pointer: false,
                                    runtime_dim_upper: vec![],
                                    is_class: false,
                                    logical_kind: None,
                                    last_dim_assumed_size: false,
                                },
                            );
                            continue;
                        }
                    }
                    // IEEE opaque types are integer ordinals (class/round/
                    // flag) or a 16-byte FP-env save buffer (status), under
                    // the hood (l09), not struct storage.
                    if let Some(kind) =
                        crate::sema::resolve::type_resolution::ieee_opaque_int_kind(type_name)
                    {
                        let scalar_ty = IrType::int_from_kind(kind);
                        let addr = b.alloca(scalar_ty.clone());
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: scalar_ty,
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::None,
                                derived_type: None,
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                        continue;
                    }
                    // Derived type variable: allocate struct-sized byte array.
                    if let Some(type_name) = declared_derived_layout_name.as_ref() {
                        let layout = type_layouts
                            .get(type_name)
                            .expect("canonical derived layout should be registered");
                        let struct_ty =
                            IrType::Array(Box::new(IrType::Int(IntWidth::I8)), layout.size as u64);
                        let addr = b.alloca(struct_ty);
                        if derived_layout_needs_runtime_initialization(layout, type_layouts) {
                            initialize_derived_storage(b, addr, layout, type_layouts);
                        }
                        // Store the derived type name in the ty field for component access lookup.
                        // Use Ptr<i8> as a marker — the type_layouts registry is used for field resolution.
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::None,
                                derived_type: Some(type_name.clone()),
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                    } else {
                        // Unknown derived type — fall back to 8-byte alloca.
                        let addr = b.alloca(IrType::Int(IntWidth::I64));
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: elem_ty.clone(),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::None,
                                derived_type: None,
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            },
                        );
                    }
                } else if is_pointer_attr && array_spec.is_none() {
                    // Scalar Fortran POINTER: allocate a pointer slot
                    // (`alloca ptr<elem_ty>`) that holds the address
                    // of whatever the pointer is currently associated
                    // with.  `=>` stores into this slot; plain `=`
                    // dereferences it; reads load twice.  The slot
                    // starts null so that ASSOCIATED() returns
                    // false before the first `=>`.
                    let addr = b.alloca(IrType::Ptr(Box::new(elem_ty.clone())));
                    // Memset the slot to zero so unassociated pointers
                    // compare null.  Eight bytes matches the ARM64
                    // pointer width.
                    let zero_byte = b.const_i32(0);
                    let eight = b.const_i64(8);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![addr, zero_byte, eight],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    locals.insert(
                        key,
                        LocalInfo {
                            addr,
                            ty: elem_ty.clone(),
                            dims: vec![],
                            allocatable: false,
                            descriptor_arg: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: None,
                            inline_const: None,
                            is_pointer: true,
                            runtime_dim_upper: vec![],
                            is_class: false,
                            logical_kind: None,
                            last_dim_assumed_size: false,
                        },
                    );
                } else {
                    // Scalar variable. Three sub-cases:
                    //   (a) PARAMETER-attributed and folds → inline
                    //       at every use site. No alloca, no global,
                    //       no .data slot. Audit MAJOR-4.
                    //   (b) Has a const-evaluable init but isn't a
                    //       parameter → SAVE-promote to a module
                    //       global (F2018 §8.5.16 implicit SAVE).
                    //   (c) Plain alloca, no init.
                    let is_parameter = attrs.iter().any(|a| matches!(a, Attribute::Parameter))
                        || parameter_inits.contains_key(&key);

                    if is_parameter {
                        let folded = init_expr
                            .and_then(|e| {
                                eval_const_scalar_with_decl_scope(e, decls, &param_consts, st)
                                    .or_else(|| param_consts.get(&key).copied())
                                    .or_else(|| symbol_table_parameter_const(st, &key))
                            })
                            .map(|raw| clamp_const_to_type(raw, &elem_ty));
                        if let Some(value) = folded {
                            // Sentinel alloca — never read.
                            let addr = b.alloca(elem_ty.clone());
                            locals.insert(
                                key,
                                LocalInfo {
                                    addr,
                                    ty: elem_ty.clone(),
                                    dims: vec![],
                                    allocatable: false,
                                    descriptor_arg: false,
                                    by_ref: false,
                                    char_kind: CharKind::None,
                                    derived_type: None,
                                    inline_const: Some(value),
                                    is_pointer: false,
                                    runtime_dim_upper: vec![],
                                    is_class: false,
                                    logical_kind: None,
                                    last_dim_assumed_size: false,
                                },
                            );
                            continue;
                        }
                        // Fall through to the SAVE path if the
                        // parameter init can't be folded — at least
                        // semantics are preserved.
                    }

                    if let Some(init) = init_expr
                        .and_then(|e| eval_const_global_init(e, &param_consts, Some(&elem_ty)))
                    {
                        let global_name = save_global_name(func_name, &key);
                        pending_globals.push(PendingGlobal {
                            global: Global {
                                name: global_name.clone(),
                                ty: elem_ty.clone(),
                                initializer: Some(init),
                            },
                        });
                        let addr = b.global_addr(&global_name, elem_ty.clone());
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: elem_ty.clone(),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::None,
                                derived_type: None,
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: type_spec_logical_kind(
                                    type_spec,
                                    Some(&param_consts),
                                    Some(st),
                                ),
                                last_dim_assumed_size: false,
                            },
                        );
                    } else {
                        let addr = b.alloca(elem_ty.clone());
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: elem_ty.clone(),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: CharKind::None,
                                derived_type: None,
                                inline_const: None,
                                is_pointer: false,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: type_spec_logical_kind(
                                    type_spec,
                                    Some(&param_consts),
                                    Some(st),
                                ),
                                last_dim_assumed_size: false,
                            },
                        );
                    }
                }
            }
        }
    }

    // VOLATILE is a property of the referenced storage, not of one
    // particular load/store syntax. Mark the address once after all
    // declarations have been installed so every lowering helper (scalar,
    // array, component, descriptor, and pointer paths) emits explicit
    // volatile memory operations through FuncBuilder.
    let mut volatile_names = std::collections::HashSet::new();
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl {
                attrs, entities, ..
            } if attrs.iter().any(|attr| matches!(attr, Attribute::Volatile)) => {
                volatile_names.extend(entities.iter().map(|entity| entity.name.to_lowercase()));
            }
            Decl::AttributeStmt {
                attr: Attribute::Volatile,
                entities,
            } => {
                volatile_names.extend(entities.iter().map(|name| name.to_lowercase()));
            }
            _ => {}
        }
    }
    for name in volatile_names {
        let Some(info) = locals.get(&name) else {
            continue;
        };
        if info.by_ref {
            b.mark_indirect_volatile_address(info.addr);
        } else {
            b.mark_volatile_address(info.addr);
        }
    }
}

fn rewrite_heap_promoted_declared_bounds(b: &mut FuncBuilder, desc: ValueId, dims: &[(i64, i64)]) {
    if dims.is_empty() {
        return;
    }

    let rank = b.const_i32(dims.len() as i32);
    store_byte_aggregate_field(b, desc, 16, IrType::Int(IntWidth::I32), rank);

    let mut stride = 1_i64;
    for (idx, (lower, extent)) in dims.iter().copied().enumerate() {
        let offset = 24 + (idx as i64) * 24;
        let upper = if extent <= 0 {
            lower.saturating_sub(1)
        } else {
            lower.saturating_add(extent).saturating_sub(1)
        };
        let lower_val = b.const_i64(lower);
        let upper_val = b.const_i64(upper);
        let stride_val = b.const_i64(stride);
        store_byte_aggregate_field(b, desc, offset, IrType::Int(IntWidth::I64), lower_val);
        store_byte_aggregate_field(b, desc, offset + 8, IrType::Int(IntWidth::I64), upper_val);
        store_byte_aggregate_field(b, desc, offset + 16, IrType::Int(IntWidth::I64), stride_val);
        stride = stride.saturating_mul(extent.max(1));
    }
}
