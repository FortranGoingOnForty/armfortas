//! Allocation of local variables from declarations.
//!
//! Extracted from `core.rs` in Sprint 11 Stage E. Pure mechanical
//! move — behavior unchanged.

use std::collections::{HashMap, HashSet};

use crate::ast::decl::{ArraySpec, DataValue, Decl, TypeSpec};
use crate::ast::expr::{AcValue, Expr, SectionSubscript, SpannedExpr};
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;
use crate::sema::symtab::SymbolTable;

use super::const_scalar::{clamp_const_to_type, ConstScalar};
use super::core::*;
use super::ctx::{current_proc_scope, CharKind, LocalInfo};
use super::helpers::{clamp_nonnegative_i64, widen_to_i64};

#[derive(Debug, Clone)]
struct StaticDataInitPlan {
    slots: Vec<Option<SpannedExpr>>,
    valid: bool,
}

fn static_data_object_count(dims: &[(i64, i64)]) -> Option<usize> {
    if dims.is_empty() {
        return Some(1);
    }
    dims.iter().try_fold(1usize, |total, (_, extent)| {
        let extent = usize::try_from(*extent).ok()?;
        (extent > 0).then_some(())?;
        total.checked_mul(extent)
    })
}

fn static_data_int_expr(value: i64, span: crate::lexer::Span) -> SpannedExpr {
    crate::ast::Spanned::new(
        Expr::IntegerLiteral {
            text: value.to_string(),
            kind: None,
        },
        span,
    )
}

fn eval_static_data_int(
    expr: &SpannedExpr,
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
) -> Option<i64> {
    eval_const_int_in_scope_or_any_scope(expr, param_consts, st)
        .or_else(|| eval_const_int_in_scope(expr, param_consts))
        .or_else(|| eval_const_int(expr))
}

fn collect_static_data_object_names(expr: &SpannedExpr, names: &mut HashSet<String>) {
    match &expr.node {
        Expr::Name { name } => {
            names.insert(name.to_lowercase());
        }
        Expr::FunctionCall { callee, .. } => {
            if let Expr::Name { name } = &callee.node {
                names.insert(name.to_lowercase());
            }
        }
        Expr::ArrayConstructor { values, .. } => {
            fn collect_ac_value(value: &AcValue, names: &mut HashSet<String>) {
                match value {
                    AcValue::Expr(expr) => collect_static_data_object_names(expr, names),
                    AcValue::ImpliedDo(ido) => {
                        for value in &ido.values {
                            collect_ac_value(value, names);
                        }
                    }
                }
            }
            for value in values {
                collect_ac_value(value, names);
            }
        }
        Expr::ParenExpr { inner } => collect_static_data_object_names(inner, names),
        _ => {}
    }
}

fn static_data_target(
    expr: &SpannedExpr,
    shapes: &HashMap<String, Vec<(i64, i64)>>,
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
) -> Option<Vec<(String, usize)>> {
    match &expr.node {
        Expr::ParenExpr { inner } => static_data_target(inner, shapes, param_consts, st),
        Expr::Name { name } => {
            let key = name.to_lowercase();
            let dims = shapes.get(&key)?;
            let count = static_data_object_count(dims)?;
            Some((0..count).map(|index| (key.clone(), index)).collect())
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let key = name.to_lowercase();
            let dims = shapes.get(&key)?;
            if dims.is_empty() || args.len() != dims.len() {
                return None;
            }
            let mut linear = 0usize;
            let mut stride = 1usize;
            for (arg, (lower, extent)) in args.iter().zip(dims) {
                let SectionSubscript::Element(index_expr) = &arg.value else {
                    return None;
                };
                let index = eval_static_data_int(index_expr, param_consts, st)?;
                if index < *lower || index >= lower.checked_add(*extent)? {
                    return None;
                }
                let offset = usize::try_from(index - lower).ok()?;
                linear = linear.checked_add(offset.checked_mul(stride)?)?;
                stride = stride.checked_mul(usize::try_from(*extent).ok()?)?;
            }
            Some(vec![(key, linear)])
        }
        _ => None,
    }
}

fn expand_static_data_ac_value(
    value: &AcValue,
    substitutions: &HashMap<String, &SpannedExpr>,
    shapes: &HashMap<String, Vec<(i64, i64)>>,
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
    targets: &mut Vec<(String, usize)>,
) -> bool {
    match value {
        AcValue::Expr(expr) => {
            let expr = super::expr::substitute_names_in_expr(expr, substitutions);
            let Some(mut expanded) = static_data_target(&expr, shapes, param_consts, st) else {
                return false;
            };
            targets.append(&mut expanded);
            true
        }
        AcValue::ImpliedDo(ido) => {
            let start_expr = super::expr::substitute_names_in_expr(&ido.start, substitutions);
            let end_expr = super::expr::substitute_names_in_expr(&ido.end, substitutions);
            let Some(start) = eval_static_data_int(&start_expr, param_consts, st) else {
                return false;
            };
            let Some(end) = eval_static_data_int(&end_expr, param_consts, st) else {
                return false;
            };
            let step = match &ido.step {
                Some(step) => {
                    let step = super::expr::substitute_names_in_expr(step, substitutions);
                    let Some(step) = eval_static_data_int(&step, param_consts, st) else {
                        return false;
                    };
                    step
                }
                None => 1,
            };
            if step == 0 {
                return false;
            }
            let mut index = start;
            while if step > 0 { index <= end } else { index >= end } {
                let replacement = static_data_int_expr(index, ido.start.span);
                let mut nested = substitutions.clone();
                nested.insert(ido.var.to_lowercase(), &replacement);
                for value in &ido.values {
                    if !expand_static_data_ac_value(
                        value,
                        &nested,
                        shapes,
                        param_consts,
                        st,
                        targets,
                    ) {
                        return false;
                    }
                }
                let Some(next) = index.checked_add(step) else {
                    return false;
                };
                index = next;
            }
            true
        }
    }
}

fn expand_static_data_objects(
    objects: &[SpannedExpr],
    shapes: &HashMap<String, Vec<(i64, i64)>>,
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
) -> Option<Vec<(String, usize)>> {
    let mut targets = Vec::new();
    let substitutions = HashMap::new();
    for object in objects {
        if let Expr::ArrayConstructor { values, .. } = &object.node {
            for value in values {
                if !expand_static_data_ac_value(
                    value,
                    &substitutions,
                    shapes,
                    param_consts,
                    st,
                    &mut targets,
                ) {
                    return None;
                }
            }
        } else {
            let mut expanded = static_data_target(object, shapes, param_consts, st)?;
            targets.append(&mut expanded);
        }
    }
    Some(targets)
}

fn expand_static_data_values(
    values: &[DataValue],
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
) -> Option<Vec<SpannedExpr>> {
    let mut expanded = Vec::new();
    for value in values {
        match value {
            DataValue::Expr(expr) => expanded.push(expr.clone()),
            DataValue::Repeat { count, value } => {
                let repeat = eval_static_data_int(count, param_consts, st)?;
                let repeat = usize::try_from(repeat).ok()?;
                expanded.try_reserve(repeat).ok()?;
                expanded.extend((0..repeat).map(|_| value.clone()));
            }
        }
    }
    Some(expanded)
}

fn collect_static_data_init_plans(
    decls: &[crate::ast::decl::SpannedDecl],
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
) -> HashMap<String, StaticDataInitPlan> {
    let mut shapes = HashMap::new();
    for decl in decls {
        let Decl::TypeDecl {
            attrs, entities, ..
        } = &decl.node
        else {
            continue;
        };
        if attrs.iter().any(|attr| {
            matches!(
                attr,
                crate::ast::decl::Attribute::Allocatable | crate::ast::decl::Attribute::Pointer
            )
        }) {
            continue;
        }
        let attr_dims = attrs.iter().find_map(|attr| match attr {
            crate::ast::decl::Attribute::Dimension(specs) => Some(specs),
            _ => None,
        });
        for entity in entities {
            let dims = match entity.array_spec.as_ref().or(attr_dims) {
                Some(specs) => {
                    if array_spec_has_runtime_bounds(specs, param_consts, Some(st)) {
                        continue;
                    }
                    let dims = extract_array_dims_with_init(
                        specs,
                        entity.init.as_ref(),
                        param_consts,
                        Some(st),
                    );
                    if static_data_object_count(&dims).is_none() {
                        continue;
                    }
                    dims
                }
                None => Vec::new(),
            };
            shapes.entry(entity.name.to_lowercase()).or_insert(dims);
        }
    }

    let mut plans: HashMap<String, StaticDataInitPlan> = HashMap::new();
    for decl in decls {
        let Decl::DataStmt { sets } = &decl.node else {
            continue;
        };
        for set in sets {
            let mut raw_names = HashSet::new();
            for object in &set.objects {
                collect_static_data_object_names(object, &mut raw_names);
            }
            let targets = expand_static_data_objects(&set.objects, &shapes, param_consts, st);
            let values = expand_static_data_values(&set.values, param_consts, st);
            let Some((targets, values)) = targets.zip(values) else {
                for key in raw_names {
                    if let Some(dims) = shapes.get(&key) {
                        let count = static_data_object_count(dims).unwrap_or(1);
                        plans
                            .entry(key)
                            .or_insert_with(|| StaticDataInitPlan {
                                slots: vec![None; count],
                                valid: false,
                            })
                            .valid = false;
                    }
                }
                continue;
            };
            if targets.len() != values.len() {
                for key in raw_names {
                    if let Some(dims) = shapes.get(&key) {
                        let count = static_data_object_count(dims).unwrap_or(1);
                        plans
                            .entry(key)
                            .or_insert_with(|| StaticDataInitPlan {
                                slots: vec![None; count],
                                valid: false,
                            })
                            .valid = false;
                    }
                }
                continue;
            }
            for ((key, index), value) in targets.into_iter().zip(values) {
                let Some(dims) = shapes.get(&key) else {
                    continue;
                };
                let count = static_data_object_count(dims).unwrap_or(1);
                let plan = plans.entry(key).or_insert_with(|| StaticDataInitPlan {
                    slots: vec![None; count],
                    valid: true,
                });
                if index >= plan.slots.len() || plan.slots[index].is_some() {
                    plan.valid = false;
                    continue;
                }
                plan.slots[index] = Some(value);
            }
        }
    }
    plans
}

fn eval_numeric_data_array_init(
    plan: &StaticDataInitPlan,
    elem_ty: &IrType,
    total: i64,
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
) -> Option<GlobalInit> {
    let total = usize::try_from(total).ok()?;
    if !plan.valid || plan.slots.len() != total {
        return None;
    }
    if is_complex_ty(elem_ty) {
        let mut lanes = vec![0.0; total.checked_mul(2)?];
        for (index, expr) in plan.slots.iter().enumerate() {
            let Some(expr) = expr else { continue };
            let values = match eval_const_complex_global_init(expr, param_consts, elem_ty, st) {
                Some(GlobalInit::FloatArray(values)) if values.len() == 2 => values,
                _ => {
                    let scalar = eval_const_global_init_with_any_scope(
                        expr,
                        param_consts,
                        complex_component_type(elem_ty),
                        st,
                    )?;
                    let real = match scalar {
                        GlobalInit::Float(value) => value,
                        GlobalInit::Int(value) => value as f64,
                        _ => return None,
                    };
                    vec![real, 0.0]
                }
            };
            lanes[index * 2] = values[0];
            lanes[index * 2 + 1] = values[1];
        }
        return Some(GlobalInit::FloatArray(lanes));
    }
    if matches!(elem_ty, IrType::Float(_)) {
        let mut values = vec![0.0; total];
        for (index, expr) in plan.slots.iter().enumerate() {
            let Some(expr) = expr else { continue };
            values[index] =
                match eval_const_global_init_with_any_scope(expr, param_consts, Some(elem_ty), st)?
                {
                    GlobalInit::Float(value) => value,
                    GlobalInit::Int(value) => value as f64,
                    _ => return None,
                };
        }
        return Some(GlobalInit::FloatArray(values));
    }
    if matches!(elem_ty, IrType::Bool | IrType::Int(_)) {
        let mut values = vec![0; total];
        for (index, expr) in plan.slots.iter().enumerate() {
            let Some(expr) = expr else { continue };
            values[index] =
                match eval_const_global_init_with_any_scope(expr, param_consts, Some(elem_ty), st)?
                {
                    GlobalInit::Int(value) => value,
                    GlobalInit::Float(value) => value as i128,
                    _ => return None,
                };
        }
        return Some(GlobalInit::IntArray(values));
    }
    None
}

fn complex_component_type(ty: &IrType) -> Option<&IrType> {
    match ty {
        IrType::Array(component, 2) if matches!(component.as_ref(), IrType::Float(_)) => {
            Some(component)
        }
        _ => None,
    }
}

fn eval_character_data_array_init(
    plan: &StaticDataInitPlan,
    total: i64,
    len: i64,
    param_consts: &HashMap<String, ConstScalar>,
    param_char_consts: &HashMap<String, Vec<u8>>,
) -> Option<GlobalInit> {
    let total = usize::try_from(total).ok()?;
    let len = usize::try_from(len).ok()?;
    if !plan.valid || plan.slots.len() != total {
        return None;
    }
    let mut bytes = vec![b' '; total.checked_mul(len)?];
    for (index, expr) in plan.slots.iter().enumerate() {
        let Some(expr) = expr else { continue };
        let value = eval_const_char_bytes(expr, param_consts, param_char_consts)?;
        let start = index.checked_mul(len)?;
        let end = start.checked_add(len)?;
        for (dst, src) in bytes[start..end].iter_mut().zip(value) {
            *dst = src;
        }
    }
    Some(GlobalInit::String(bytes))
}

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

fn lower_explicit_shape_dim_buffer(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    specs: &[ArraySpec],
    param_consts: &HashMap<String, ConstScalar>,
    st: &SymbolTable,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> ValueId {
    if specs.is_empty() {
        return b.const_i64(0);
    }

    let dim_buf = b.alloca(IrType::Array(
        Box::new(IrType::Int(IntWidth::I8)),
        (specs.len() * 24) as u64,
    ));
    let one_i64 = b.const_i64(1);
    let mut running_stride = one_i64;
    for (i, spec) in specs.iter().enumerate() {
        let (lo64, up64) = match spec {
            ArraySpec::Explicit { lower, upper } => {
                let lo64 = lower
                    .as_ref()
                    .and_then(|expr| eval_const_array_bound(expr, param_consts, Some(st)))
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
                let up64 = eval_const_array_bound(upper, param_consts, Some(st))
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
        if i + 1 < specs.len() {
            let span = b.isub(up64, lo64);
            let extent = b.iadd(span, one_i64);
            running_stride = b.imul(running_stride, extent);
        }
    }
    dim_buf
}

fn saved_zero_storage(
    b: &mut FuncBuilder,
    pending_globals: &mut Vec<PendingGlobal>,
    func_name: &str,
    local_name: &str,
    storage_ty: IrType,
) -> ValueId {
    let global_name = save_global_name(func_name, local_name);
    pending_globals.push(PendingGlobal {
        global: Global {
            name: global_name.clone(),
            ty: storage_ty.clone(),
            initializer: Some(GlobalInit::Zero),
        },
    });
    b.global_addr(&global_name, storage_ty)
}

fn alloc_zeroed_or_saved_storage(
    b: &mut FuncBuilder,
    pending_globals: &mut Vec<PendingGlobal>,
    func_name: &str,
    local_name: &str,
    storage_ty: IrType,
    byte_size: i64,
    is_saved: bool,
) -> ValueId {
    if is_saved {
        return saved_zero_storage(b, pending_globals, func_name, local_name, storage_ty);
    }

    let addr = b.alloca(storage_ty);
    let zero = b.const_i32(0);
    let size = b.const_i64(byte_size);
    b.call(
        FuncRef::External("memset".into()),
        vec![addr, zero, size],
        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
    );
    addr
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
    let data_init_plans = collect_static_data_init_plans(decls, &param_consts, st);
    let save_all = decls.iter().any(|decl| {
        matches!(
            &decl.node,
            Decl::AttributeStmt {
                attr: Attribute::Save,
                entities,
            } if entities.is_empty()
        )
    });

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
                // A typed EXTERNAL declaration describes a procedure result,
                // not a data object. Standalone legacy spelling commonly puts
                // EXTERNAL in a later attribute statement, so consult sema's
                // merged symbol instead of looking only at this TypeDecl.
                if current_proc_scope()
                    .and_then(|scope_id| st.lookup_in(scope_id, &key))
                    .is_some_and(|symbol| {
                        symbol.attrs.external
                            && symbol.kind != crate::sema::symtab::SymbolKind::ProcedurePointer
                    })
                {
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
                let data_init_plan = data_init_plans.get(&key);
                let data_init_expr = if array_spec.is_none()
                    && !is_allocatable
                    && !is_pointer_attr
                    && !matches!(type_spec, TypeSpec::Type(_) | TypeSpec::Class(_))
                {
                    data_init_plan
                        .filter(|plan| plan.valid)
                        .and_then(|plan| plan.slots.first())
                        .and_then(Option::as_ref)
                } else {
                    None
                };
                let has_data_init = data_init_plan.is_some();
                let is_saved =
                    save_all || attrs.iter().any(|a| matches!(a, Attribute::Save)) || has_data_init;

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
                    let addr = alloc_zeroed_or_saved_storage(
                        b,
                        pending_globals,
                        func_name,
                        &key,
                        desc_ty,
                        392,
                        is_saved,
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
                        let slot_ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)));
                        let addr = alloc_zeroed_or_saved_storage(
                            b,
                            pending_globals,
                            func_name,
                            &key,
                            slot_ty,
                            8,
                            is_saved,
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
                    let addr = alloc_zeroed_or_saved_storage(
                        b,
                        pending_globals,
                        func_name,
                        &key,
                        desc_ty,
                        392,
                        is_saved,
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
                    let addr = alloc_zeroed_or_saved_storage(
                        b,
                        pending_globals,
                        func_name,
                        &key,
                        desc_ty,
                        32,
                        is_saved,
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
                        // Column-major stride accumulator: dim[k].stride =
                        // product(extents[0..k]).
                        let dim_buf = lower_explicit_shape_dim_buffer(
                            b,
                            locals,
                            specs,
                            &param_consts,
                            st,
                            type_layouts,
                        );

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
                        let static_init = if !is_parameter && total_size > 0 {
                            match (init_expr, data_init_plan) {
                                (Some(expr), _) => eval_const_char_array_init(
                                    expr,
                                    total_size,
                                    len,
                                    &param_consts,
                                    &param_char_consts,
                                    Some(st),
                                    current_proc_scope(),
                                    Some(type_layouts),
                                ),
                                (None, Some(plan)) => eval_character_data_array_init(
                                    plan,
                                    total_size,
                                    len,
                                    &param_consts,
                                    &param_char_consts,
                                ),
                                (None, None) if is_saved => Some(GlobalInit::Zero),
                                (None, None) => None,
                            }
                        } else {
                            None
                        };
                        if let Some(initializer) = static_init {
                            let arr_ty =
                                IrType::Array(Box::new(elem_ty.clone()), total_size as u64);
                            let global_name = save_global_name(func_name, &key);
                            pending_globals.push(PendingGlobal {
                                global: Global {
                                    name: global_name.clone(),
                                    ty: arr_ty.clone(),
                                    initializer: Some(initializer),
                                },
                            });
                            let addr = b.global_addr(&global_name, arr_ty);
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
                            continue;
                        }
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
                        let slot_ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)));
                        let addr = alloc_zeroed_or_saved_storage(
                            b,
                            pending_globals,
                            func_name,
                            &key,
                            slot_ty,
                            8,
                            is_saved,
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
                        let static_init_expr = init_expr.or(data_init_expr);
                        if !is_parameter
                            && array_spec.is_none()
                            && (is_saved || static_init_expr.is_some())
                        {
                            let mut bytes = vec![b' '; len.max(0) as usize + 1];
                            let mut const_init = static_init_expr.is_none();
                            if let Some(expr) = static_init_expr {
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
                    // Allocatable variable: a zero-initialized 392-byte
                    // descriptor. SAVE'd descriptors live in static storage;
                    // ordinary descriptors remain per-activation allocas.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392);
                    let addr = alloc_zeroed_or_saved_storage(
                        b,
                        pending_globals,
                        func_name,
                        &key,
                        desc_ty,
                        392,
                        is_saved,
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
                    // Keep the declared character length on rank-0 fixed-length
                    // allocatables too. Encoding the length only in `ty` loses
                    // CHARACTER(1), whose storage type is the same i8 used by
                    // INTEGER(1), and makes `value(1:1)` look like a function
                    // call instead of a substring.
                    let char_kind = match char_len {
                        Some(len) => CharKind::Fixed(len),
                        None => CharKind::None,
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

                            // A runtime character length does not imply a
                            // static array extent.  Build the descriptor from
                            // every declared bound so `character(len(items))
                            // :: tail(count - 1)` allocates COUNT-1 elements,
                            // rather than the `(1, 0)` static-analysis
                            // sentinel collapsing to one element.
                            let dim_buf = lower_explicit_shape_dim_buffer(
                                b,
                                locals,
                                specs,
                                &param_consts,
                                st,
                                type_layouts,
                            );
                            let rank = b.const_i32(specs.len() as i32);
                            let stat_slot = b.alloca(IrType::Int(IntWidth::I32));
                            b.call(
                                FuncRef::External("afs_allocate_array".into()),
                                vec![addr, len_val, rank, dim_buf, stat_slot],
                                IrType::Void,
                            );

                            let base = b
                                .load_typed(addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                            let n = b.call(
                                FuncRef::External("afs_array_size".into()),
                                vec![addr],
                                IrType::Int(IntWidth::I64),
                            );
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

                    // A compile-time-shaped local with SAVE has static storage
                    // duration. Keep it out of both the small-array alloca path
                    // and the large-array descriptor/heap path: recreating either
                    // form on every procedure entry loses values between calls.
                    // An initializer implies SAVE too (F2018 8.5.16.4).
                    let static_init =
                        if !is_parameter && total_size > 0 && (is_saved || init_expr.is_some()) {
                            if let Some(type_name) = array_derived_type.as_deref() {
                                if is_saved
                                    && init_expr.is_none()
                                    && !type_layouts.get(type_name).is_some_and(|layout| {
                                        derived_layout_has_procedure_pointer_defaults(
                                            layout,
                                            type_layouts,
                                        )
                                    })
                                {
                                    type_layouts.get(type_name).map(|layout| {
                                        eval_const_derived_global_init(
                                            layout,
                                            total_size as usize,
                                            type_layouts,
                                        )
                                        .unwrap_or(GlobalInit::Zero)
                                    })
                                } else {
                                    None
                                }
                            } else {
                                match (init_expr, data_init_plan) {
                                    (Some(expr), _) => eval_const_array_init(
                                        expr,
                                        &array_elem_ty,
                                        total_size,
                                        &param_consts,
                                        &HashMap::new(),
                                        &HashMap::new(),
                                    ),
                                    (None, Some(plan)) => eval_numeric_data_array_init(
                                        plan,
                                        &array_elem_ty,
                                        total_size,
                                        &param_consts,
                                        st,
                                    ),
                                    (None, None) => Some(GlobalInit::Zero),
                                }
                            }
                        } else {
                            None
                        };
                    if let Some(initializer) = static_init {
                        let arr_ty =
                            IrType::Array(Box::new(array_elem_ty.clone()), total_size as u64);
                        let global_name = save_global_name(func_name, &key);
                        pending_globals.push(PendingGlobal {
                            global: Global {
                                name: global_name.clone(),
                                ty: arr_ty.clone(),
                                initializer: Some(initializer),
                            },
                        });
                        let addr = b.global_addr(&global_name, arr_ty);
                        locals.insert(
                            key,
                            LocalInfo {
                                addr,
                                ty: array_elem_ty,
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
                        continue;
                    }

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
                            let addr = if is_saved {
                                saved_zero_storage(
                                    b,
                                    pending_globals,
                                    func_name,
                                    &key,
                                    scalar_ty.clone(),
                                )
                            } else {
                                b.alloca(scalar_ty.clone())
                            };
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
                        let addr = if is_saved {
                            saved_zero_storage(
                                b,
                                pending_globals,
                                func_name,
                                &key,
                                scalar_ty.clone(),
                            )
                        } else {
                            b.alloca(scalar_ty.clone())
                        };
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
                        let static_init = if !is_parameter
                            && (is_saved || init_expr.is_some())
                            && !derived_layout_has_procedure_pointer_defaults(layout, type_layouts)
                        {
                            init_expr
                                .and_then(|expr| {
                                    eval_const_derived_ctor_global_init(
                                        type_name,
                                        expr,
                                        type_layouts,
                                        &param_consts,
                                        &param_char_consts,
                                        st,
                                    )
                                })
                                .or_else(|| {
                                    (is_saved && init_expr.is_none()).then(|| {
                                        eval_const_derived_global_init(layout, 1, type_layouts)
                                            .unwrap_or(GlobalInit::Zero)
                                    })
                                })
                        } else {
                            None
                        };
                        let addr = if let Some(initializer) = static_init {
                            let global_name = save_global_name(func_name, &key);
                            pending_globals.push(PendingGlobal {
                                global: Global {
                                    name: global_name.clone(),
                                    ty: struct_ty.clone(),
                                    initializer: Some(initializer),
                                },
                            });
                            b.global_addr(&global_name, struct_ty)
                        } else {
                            let addr = b.alloca(struct_ty);
                            if derived_layout_needs_runtime_initialization(layout, type_layouts) {
                                initialize_derived_storage(b, addr, layout, type_layouts);
                            }
                            addr
                        };
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
                    let slot_ty = IrType::Ptr(Box::new(elem_ty.clone()));
                    let addr = alloc_zeroed_or_saved_storage(
                        b,
                        pending_globals,
                        func_name,
                        &key,
                        slot_ty,
                        8,
                        is_saved,
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

                    let static_init_expr = init_expr.or(data_init_expr);
                    let static_init = static_init_expr
                        .and_then(|e| {
                            if is_complex_ty(&elem_ty) {
                                eval_const_complex_global_init(e, &param_consts, &elem_ty, st)
                            } else {
                                eval_const_global_init(e, &param_consts, Some(&elem_ty))
                            }
                        })
                        .or_else(|| {
                            (!is_parameter
                                && is_saved
                                && static_init_expr.is_none()
                                && !has_data_init)
                                .then_some(GlobalInit::Zero)
                        });
                    if let Some(init) = static_init {
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
