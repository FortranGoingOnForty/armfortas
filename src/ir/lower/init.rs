//! Initializer lowering for declared variables.
//!
//! Extracted from `core.rs` in Sprint 11 Stage E. Pure mechanical
//! move — behavior unchanged.

use std::collections::HashMap;

use crate::ast::decl::{Attribute, Decl, TypeSpec};
use crate::ast::expr::Expr;
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;
use crate::sema::symtab::{ScopeId, ScopeKind, SymbolTable};

use super::const_scalar::{eval_const_scalar, materialize_const_scalar, ConstScalar};
use super::core::*;
use super::ctx::{CharKind, LocalInfo};
use super::helpers::coerce_to_type;

/// Lower initializer expressions for declared variables.
///
/// Handles two AST shapes:
///   1. `Decl::TypeDecl` entities with `entity.init` set. This
///      covers BOTH `integer :: x = 42` and
///      `integer, parameter :: pi = 3.14` — the parameter
///      attribute doesn't change the lowering, only sema's
///      classification of the symbol.
///   2. Standalone `Decl::ParameterStmt { pairs }`, where each
///      pair refers to an already-allocated local declared
///      elsewhere in the same decl list.
///
/// Most scalar locals with const-evaluable initializers are
/// SAVE-promoted to module globals back in `alloc_decls`; for
/// those, `is_global_addr` returns true and this pass leaves the
/// initialization to the .data section. The remaining cases this
/// pass handles are non-const initializers (rare).
///
/// Must run *after* `alloc_decls` so that all locals exist. Only
/// stores into scalar slots — array, character, derived-type, and
/// allocatable initializers have their own paths in alloc_decls.
pub(crate) fn init_decls(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    decls: &[crate::ast::decl::SpannedDecl],
    st: &SymbolTable,
    proc_scope_id: Option<ScopeId>,
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
) {
    // Pre-collect the set of GlobalAddr-defining ValueIds so the
    // inner skip check is O(1). Audit Maj-3.
    let global_addr_ids = collect_global_addr_values(b);
    let param_consts = collect_decl_param_consts_with_scope(decls, &HashMap::new(), st);
    let mut param_array_consts: HashMap<String, Vec<ConstScalar>> = HashMap::new();
    let mut param_array_elem_tys: HashMap<String, IrType> = HashMap::new();
    let mut parameter_inits: HashMap<String, &crate::ast::expr::SpannedExpr> = HashMap::new();
    for decl in decls {
        if let Decl::ParameterStmt { pairs } = &decl.node {
            for (name, expr) in pairs {
                parameter_inits.insert(name.to_lowercase(), expr);
            }
        }
    }
    for decl in decls {
        let Decl::TypeDecl {
            type_spec,
            attrs,
            entities,
        } = &decl.node
        else {
            continue;
        };
        if matches!(type_spec, TypeSpec::Character(_) | TypeSpec::Type(_)) {
            continue;
        }
        let attr_dims: Option<&Vec<crate::ast::decl::ArraySpec>> = attrs.iter().find_map(|a| {
            if let Attribute::Dimension(specs) = a {
                Some(specs)
            } else {
                None
            }
        });
        let is_parameter_decl = attrs.iter().any(|a| matches!(a, Attribute::Parameter));
        let elem_ty = lower_type_spec_with_param_consts(type_spec, Some(&param_consts), Some(st));
        for entity in entities {
            let key = entity.name.to_lowercase();
            let init_expr = entity
                .init
                .as_ref()
                .or_else(|| parameter_inits.get(&key).copied());
            let is_parameter = is_parameter_decl || parameter_inits.contains_key(&key);
            let Some(init_expr) = init_expr else {
                continue;
            };
            let Some(specs) = entity.array_spec.as_ref().or(attr_dims) else {
                continue;
            };
            if !is_parameter {
                continue;
            }
            let dims =
                extract_array_dims_with_init(specs, Some(init_expr), &param_consts, Some(st));
            let total: i64 = dims.iter().map(|(_, size)| *size).product();
            if total <= 0 {
                continue;
            }
            let Some(mut scalars) = collect_const_array_scalars(
                init_expr,
                &elem_ty,
                &param_consts,
                &param_array_consts,
                &param_array_elem_tys,
            )
            .or_else(|| {
                eval_const_scalar(init_expr, &param_consts)
                    .map(|s| coerce_scalar_to_array_lanes(s, &elem_ty))
            }) else {
                continue;
            };
            let storage_total = const_array_storage_scalar_count(&elem_ty, total).unwrap_or(total);
            if (scalars.len() as i64) > storage_total {
                continue;
            }
            let lanes_per_element =
                const_array_storage_scalar_count(&elem_ty, 1).unwrap_or(1) as usize;
            if scalars.len() == lanes_per_element && total > 1 {
                let element = scalars.clone();
                scalars.clear();
                for _ in 0..total {
                    scalars.extend_from_slice(&element);
                }
            } else {
                while (scalars.len() as i64) < storage_total {
                    scalars.push(zero_const_for_array_lane(&elem_ty));
                }
            }
            param_array_consts.insert(key, scalars);
            param_array_elem_tys.insert(entity.name.to_lowercase(), elem_ty.clone());
        }
    }
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl { entities, .. } => {
                for entity in entities {
                    let Some(init_expr) = &entity.init else {
                        continue;
                    };
                    let key = entity.name.to_lowercase();
                    let Some(info) = locals.get(&key) else {
                        continue;
                    };
                    // Dummy arguments (by_ref locals) cannot have
                    // initializers per the Fortran standard — they
                    // bind to caller storage. If sema lets one
                    // through it would be a bug; the debug_assert
                    // catches it in development without crashing
                    // release builds. Audit Min-4.
                    debug_assert!(
                        !info.by_ref,
                        "init_decls: dummy argument {:?} should not have an initializer",
                        key,
                    );
                    if info.by_ref {
                        continue;
                    }

                    // Array entity with an array constructor init:
                    // store each literal element into the slot.
                    // Only stack/non-allocatable arrays are handled
                    // here; allocatable arrays would need their
                    // descriptor allocated first.
                    if !info.dims.is_empty()
                        && !info.allocatable
                        && matches!(info.char_kind, CharKind::None)
                        && info.derived_type.is_none()
                    {
                        if let Expr::ArrayConstructor { values, .. } = &init_expr.node {
                            store_ac_values_into(
                                b,
                                locals,
                                info.addr,
                                &info.ty,
                                info.derived_type.as_deref(),
                                values,
                                st,
                                type_layouts,
                                None,
                                None,
                                None,
                            );
                        } else if let Some(values) =
                            super::core::extract_reshape_source_ac(&init_expr.node)
                        {
                            // F2018 §16.9.169 RESHAPE used as a declared
                            // initializer for a fixed-shape stack array.
                            // The source AC is laid out column-major into
                            // the destination; for a contiguous source the
                            // reshape is a pure reinterpretation, so we
                            // can store the flat element list straight
                            // into the slot via the existing AC writer.
                            // Pre-fix `reshape([...], [...])` initializers
                            // were silently dropped here, leaving every
                            // rank-2+ stack array with garbage data — every
                            // example that did `real :: y(2,3) =
                            // reshape([1.,2.,3.,4.,5.,6.], [2,3])` saw
                            // y(1,1) come back as a junk float.
                            store_ac_values_into(
                                b,
                                locals,
                                info.addr,
                                &info.ty,
                                info.derived_type.as_deref(),
                                values,
                                st,
                                type_layouts,
                                None,
                                None,
                                None,
                            );
                        } else if let Some(values) =
                            super::core::extract_transpose_reshape_source_ac(&init_expr.node, st)
                        {
                            store_ac_values_into(
                                b,
                                locals,
                                info.addr,
                                &info.ty,
                                info.derived_type.as_deref(),
                                &values,
                                st,
                                type_layouts,
                                None,
                                None,
                                None,
                            );
                        } else if matches!(
                            &init_expr.node,
                            Expr::IntegerLiteral { .. }
                                | Expr::RealLiteral { .. }
                                | Expr::LogicalLiteral { .. }
                        ) && !is_complex_ty(&info.ty)
                        {
                            // F2018 §7.6.6: scalar literal initializer broadcast
                            // to every element of the array. Previously this
                            // path skipped non-AC initializers and left the
                            // stack array uninitialized — `logical :: a(4)
                            // = .true.` returned all-junk for any array
                            // size > 0. Lower the literal once, then store
                            // it at each element offset.  Restricted to
                            // literal scalars: compound expressions like
                            // `reshape(...)` return an array descriptor that
                            // must be element-wise copied via a different
                            // path.
                            let total: i64 = info.dims.iter().map(|(_, n)| *n).product();
                            if total > 0 {
                                let raw = super::expr::lower_expr(b, locals, init_expr, st);
                                let val = coerce_to_type(b, raw, &info.ty);
                                for i in 0..total {
                                    let idx = b.const_i64(i);
                                    let slot = b.gep(info.addr, vec![idx], info.ty.clone());
                                    b.store(val, slot);
                                }
                            }
                        } else if is_complex_ty(&info.ty) {
                            let _ = store_named_parameter_array_init(
                                b,
                                locals,
                                info,
                                init_expr,
                                st,
                                proc_scope_id,
                                &param_consts,
                            );
                        } else {
                            if let Some(mut scalars) = collect_const_array_scalars(
                                init_expr,
                                &info.ty,
                                &param_consts,
                                &param_array_consts,
                                &param_array_elem_tys,
                            ) {
                                let total: i64 = info.dims.iter().map(|(_, n)| *n).product();
                                if total > 1 && scalars.len() == 1 {
                                    scalars.resize(total as usize, scalars[0]);
                                }
                                if total > 0 && scalars.len() >= total as usize {
                                    for (i, scalar) in
                                        scalars.into_iter().take(total as usize).enumerate()
                                    {
                                        let idx = b.const_i64(i as i64);
                                        let slot = b.gep(info.addr, vec![idx], info.ty.clone());
                                        let val = materialize_const_scalar(b, scalar, &info.ty);
                                        b.store(val, slot);
                                    }
                                }
                            } else {
                                let _ = store_named_parameter_array_init(
                                    b,
                                    locals,
                                    info,
                                    init_expr,
                                    st,
                                    proc_scope_id,
                                    &param_consts,
                                );
                            }
                        }
                        continue;
                    }
                    if !info.dims.is_empty()
                        && !info.allocatable
                        && info.derived_type.is_none()
                        && matches!(info.char_kind, CharKind::Fixed(_))
                    {
                        if let Expr::ArrayConstructor { values, .. } = &init_expr.node {
                            if let CharKind::Fixed(len) = info.char_kind {
                                store_char_ac_values_into(
                                    b,
                                    locals,
                                    info.addr,
                                    len,
                                    values,
                                    st,
                                    type_layouts,
                                    None,
                                    None,
                                    None,
                                );
                            }
                        }
                        continue;
                    }

                    // Fixed-length character initializer: copy the
                    // literal bytes into the stack buffer with
                    // space-padding to the declared length. Previously
                    // the character arm was unconditionally skipped,
                    // leaving every `character(len=N) :: s = 'hello'`
                    // zero-initialized and silently blank at runtime
                    // (audit31 Finding 3).
                    if let CharKind::Fixed(len) = info.char_kind {
                        let (src_ptr, src_len) = lower_string_expr(b, locals, init_expr, st);
                        let dest_len = b.const_i64(len);
                        b.call(
                            FuncRef::External("afs_assign_char_fixed".into()),
                            vec![info.addr, dest_len, src_ptr, src_len],
                            IrType::Void,
                        );
                        continue;
                    }
                    if let CharKind::FixedRuntime { len_addr } = info.char_kind {
                        let (src_ptr, src_len) = lower_string_expr(b, locals, init_expr, st);
                        let (dest_ptr, dest_len) =
                            fixed_runtime_char_ptr_and_len(b, info, len_addr);
                        b.call(
                            FuncRef::External("afs_assign_char_fixed".into()),
                            vec![dest_ptr, dest_len, src_ptr, src_len],
                            IrType::Void,
                        );
                        continue;
                    }
                    if info.dims.is_empty() && !info.allocatable && !info.is_pointer {
                        if let Some(type_name) = info.derived_type.as_deref() {
                            if let Some(tl) = type_layouts {
                                if let Some(layout) = tl.get(type_name) {
                                    let src = super::expr::lower_expr_full(
                                        b,
                                        locals,
                                        init_expr,
                                        st,
                                        type_layouts,
                                        None,
                                        None,
                                        None,
                                    );
                                    let sz = b.const_i64(layout.size as i64);
                                    b.call(
                                        FuncRef::External("memcpy".into()),
                                        vec![info.addr, src, sz],
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                    continue;
                                }
                            }
                        }
                    }
                    // Other non-plain-scalar shapes are handled
                    // elsewhere (allocatables, derived types) or not
                    // at all (deferred-length character, which gets
                    // its store through afs_assign_char_deferred at
                    // the declaration's assignment lowering).
                    if !info.dims.is_empty()
                        || info.allocatable
                        || !matches!(info.char_kind, CharKind::None)
                        || info.derived_type.is_some()
                    {
                        continue;
                    }
                    // SAVE-promoted locals are backed by a module
                    // global already initialized at link time. Don't
                    // re-store on every call — that would defeat
                    // the SAVE semantics (audit MAJOR-1).
                    if global_addr_ids.contains(&info.addr) {
                        continue;
                    }
                    // Audit5 MAJOR-3: PARAMETER scalars folded by
                    // alloc_decls have inline_const set and a
                    // sentinel alloca that is never loaded — every
                    // use materializes the constant directly. The
                    // store here would be dead in the IR forever
                    // at -O0 (mem2reg cleans it up at -O1+, but
                    // we shouldn't generate dead code in the first
                    // place).
                    if info.inline_const.is_some() {
                        continue;
                    }
                    // Complex scalar init: ComplexLiteral lowers to an
                    // address of a [f32/f64 x 2] buffer. Copying a
                    // pointer into the slot (whose pointee is the
                    // 2-element array) would fail IR verification — do
                    // a byte memcpy of the inline buffer instead.
                    if is_complex_ty(&info.ty) && !info.is_pointer {
                        let src = super::expr::lower_expr(b, locals, init_expr, st);
                        let bytes = complex_byte_size(&info.ty);
                        let sz = b.const_i64(bytes);
                        b.call(
                            FuncRef::External("memcpy".into()),
                            vec![info.addr, src, sz],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        continue;
                    }
                    let val = super::expr::lower_expr(b, locals, init_expr, st);
                    let coerced = coerce_to_type(b, val, &info.ty);
                    b.store(coerced, info.addr);
                }
            }
            Decl::ParameterStmt { pairs } => {
                for (name, expr) in pairs {
                    let key = name.to_lowercase();
                    let Some(info) = locals.get(&key) else {
                        continue;
                    };
                    if let CharKind::Fixed(len) = info.char_kind {
                        let (src_ptr, src_len) = lower_string_expr(b, locals, expr, st);
                        let dest_len = b.const_i64(len);
                        b.call(
                            FuncRef::External("afs_assign_char_fixed".into()),
                            vec![info.addr, dest_len, src_ptr, src_len],
                            IrType::Void,
                        );
                        continue;
                    }
                    if let CharKind::FixedRuntime { len_addr } = info.char_kind {
                        let (src_ptr, src_len) = lower_string_expr(b, locals, expr, st);
                        let (dest_ptr, dest_len) =
                            fixed_runtime_char_ptr_and_len(b, info, len_addr);
                        b.call(
                            FuncRef::External("afs_assign_char_fixed".into()),
                            vec![dest_ptr, dest_len, src_ptr, src_len],
                            IrType::Void,
                        );
                        continue;
                    }
                    if !info.dims.is_empty()
                        || info.allocatable
                        || info.by_ref
                        || !matches!(info.char_kind, CharKind::None)
                        || info.derived_type.is_some()
                    {
                        continue;
                    }
                    // SAVE-promoted locals are backed by a module
                    // global; the initial value is already baked
                    // into .data at link time, so skip the runtime
                    // store. Audit MAJOR-1 interaction.
                    if global_addr_ids.contains(&info.addr) {
                        continue;
                    }
                    // Audit5 MAJOR-3: same dead-store skip as the
                    // TypeDecl arm above. Standalone PARAMETER
                    // statements also produce inline_const-tagged
                    // locals when alloc_decls successfully folds
                    // the value.
                    if info.inline_const.is_some() {
                        continue;
                    }
                    let val = super::expr::lower_expr(b, locals, expr, st);
                    let coerced = coerce_to_type(b, val, &info.ty);
                    b.store(coerced, info.addr);
                }
            }
            // Audit MEDIUM-3: DATA statements. Each set pairs
            // target objects with values. For the simple form
            // `data x /42/, y /3.14/`, walk objects + values
            // pairwise and emit a store per scalar Name target.
            // Implied-do object lists and value-side repetition
            // (`r*v`) are not yet supported — they fall through
            // silently and are tracked as future work.
            Decl::DataStmt { sets } => {
                for set in sets {
                    let n = set.objects.len().min(set.values.len());
                    for (target, value) in set.objects.iter().zip(set.values.iter()).take(n) {
                        let Expr::Name { name } = &target.node else {
                            continue;
                        };
                        let key = name.to_lowercase();
                        let Some(info) = locals.get(&key) else {
                            continue;
                        };
                        if !info.dims.is_empty()
                            || info.allocatable
                            || info.by_ref
                            || !matches!(info.char_kind, CharKind::None)
                            || info.derived_type.is_some()
                        {
                            continue;
                        }
                        // Don't shadow a SAVE-promoted global —
                        // its initial value is in .data already.
                        if global_addr_ids.contains(&info.addr) {
                            continue;
                        }
                        let val = super::expr::lower_expr(b, locals, value, st);
                        let coerced = coerce_to_type(b, val, &info.ty);
                        b.store(coerced, info.addr);
                    }
                }
            }
            _ => {}
        }
    }
}

fn named_initializer_key(expr: &crate::ast::expr::SpannedExpr) -> Option<String> {
    match &expr.node {
        Expr::Name { name } => Some(name.to_lowercase()),
        Expr::ParenExpr { inner } => named_initializer_key(inner),
        _ => None,
    }
}

fn store_named_parameter_array_init(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    dest: &LocalInfo,
    init_expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    proc_scope_id: Option<ScopeId>,
    param_consts: &HashMap<String, ConstScalar>,
) -> bool {
    let Some(source_key) = named_initializer_key(init_expr) else {
        return false;
    };
    let Some(source_sym) = proc_scope_id
        .and_then(|scope_id| st.lookup_in(scope_id, &source_key))
        .or_else(|| st.find_symbol_any_scope(&source_key))
    else {
        return false;
    };
    if !source_sym.attrs.parameter || source_sym.attrs.array_spec.is_empty() {
        return false;
    }

    let total: i64 = dest.dims.iter().map(|(_, n)| *n).product();
    if total <= 0 {
        return false;
    }

    if let Some(source) = locals.get(&source_key) {
        if !source.dims.is_empty()
            && !source.allocatable
            && !source.is_pointer
            && matches!(source.char_kind, CharKind::None)
            && source.derived_type.is_none()
        {
            let source_total: i64 = source.dims.iter().map(|(_, n)| *n).product();
            if source_total >= total {
                copy_parameter_array_init(b, dest, source.addr, &source.ty, total);
                return true;
            }
        }
    }

    let module_name = match &st.scope(source_sym.scope).kind {
        ScopeKind::Module(name) | ScopeKind::Submodule(name) => name,
        _ => return false,
    };
    let source_ty = source_sym
        .type_info
        .as_ref()
        .map(type_info_to_ir_type)
        .unwrap_or_else(|| dest.ty.clone());

    let source_dims = extract_array_dims(&source_sym.attrs.array_spec, param_consts, Some(st));
    let source_total: i64 = source_dims.iter().map(|(_, n)| *n).product();
    if source_total > 0 && source_total < total {
        return false;
    }

    let symbol = format!("afs_mod_{}_{}", module_name.to_lowercase(), source_key);
    let source_addr = b.global_addr(&symbol, source_ty.clone());
    copy_parameter_array_init(b, dest, source_addr, &source_ty, total);
    true
}

fn copy_parameter_array_init(
    b: &mut FuncBuilder,
    dest: &LocalInfo,
    source_addr: crate::ir::inst::ValueId,
    source_ty: &IrType,
    total: i64,
) {
    let dest_is_complex = is_complex_ty(&dest.ty);
    let source_is_complex = is_complex_ty(source_ty);
    let complex_bytes = if dest_is_complex {
        Some(b.const_i64(ir_scalar_byte_size(&dest.ty)))
    } else {
        None
    };
    let complex_fw = if dest_is_complex {
        Some(complex_float_width(&dest.ty))
    } else {
        None
    };
    for i in 0..total {
        let idx = b.const_i64(i);
        let source_slot = b.gep(source_addr, vec![idx], source_ty.clone());
        let dest_slot = b.gep(dest.addr, vec![idx], dest.ty.clone());
        if let (Some(bytes), Some(fw)) = (complex_bytes, complex_fw) {
            let source_ptr = if source_is_complex
                && complex_float_width(source_ty) == complex_float_width(&dest.ty)
            {
                source_slot
            } else {
                let raw = if source_is_complex {
                    source_slot
                } else {
                    b.load_typed(source_slot, source_ty.clone())
                };
                materialize_complex_operand(b, raw, fw)
            };
            b.call(
                FuncRef::External("memcpy".into()),
                vec![dest_slot, source_ptr, bytes],
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
            );
        } else {
            let raw = b.load_typed(source_slot, source_ty.clone());
            let val = coerce_to_type(b, raw, &dest.ty);
            b.store(val, dest_slot);
        }
    }
}
