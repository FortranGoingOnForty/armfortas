//! Lowering of Fortran statements (Stmt::*) to IR.
//!
//! Extracted from `core.rs` in Sprint 11 Stage C. Pure mechanical
//! move — behavior unchanged. The dispatcher still matches on the
//! 41 Stmt variants; future sub-stages may split per-variant.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::ast::expr::{Argument, Expr, SectionSubscript, SpannedExpr};
use crate::ast::stmt::*;
use crate::ast::Spanned;
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;

use super::core::*;
use super::ctx::{
    BlockCleanupScope, BlockScopeGuard, BlockUseGuard, CharKind, HiddenResultAbi, LocalInfo,
    LowerCtx,
};
use super::helpers::coerce_to_type;

fn is_unlimited_polymorphic_local(info: &LocalInfo) -> bool {
    info.is_class && info.derived_type.is_none()
}

fn class_star_intrinsic_source_ir_type(
    ctx: &LowerCtx,
    source_expr: &SpannedExpr,
) -> Option<IrType> {
    let ti = operator_expr_type_info(
        source_expr,
        Some(&ctx.locals),
        ctx.st,
        Some(ctx.type_layouts),
    )?;
    if matches!(ti, crate::sema::symtab::TypeInfo::Character { .. })
        || intrinsic_class_star_type_tag_for_type_info(&ti).is_none()
    {
        return None;
    }
    Some(type_info_to_ir_type(&ti))
}

fn class_star_intrinsic_source_tag_value(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    source_expr: &SpannedExpr,
) -> Option<ValueId> {
    intrinsic_class_star_type_tag_value_for_expr(
        b,
        source_expr,
        Some(&ctx.locals),
        ctx.st,
        Some(ctx.type_layouts),
    )
}

fn class_star_descriptor_source(
    b: &mut FuncBuilder,
    ctx: &LowerCtx<'_>,
    source_expr: &SpannedExpr,
) -> ValueId {
    lower_arg_descriptor_full(
        b,
        &ctx.locals,
        source_expr,
        ctx.st,
        Some(ctx.type_layouts),
        Some(ctx.internal_funcs),
        Some(ctx.contained_host_refs),
        Some(ctx.descriptor_params),
        true,
    )
}

fn root_object_name(expr: &SpannedExpr) -> Option<String> {
    match &expr.node {
        Expr::Name { name } => Some(name.clone()),
        Expr::FunctionCall { callee, .. } => root_object_name(callee),
        Expr::ComponentAccess { base, .. } => root_object_name(base),
        Expr::ParenExpr { inner } => root_object_name(inner),
        _ => None,
    }
}

fn emit_scalar_class_star_char_source_copy_on_success(
    b: &mut FuncBuilder,
    stat_addr: ValueId,
    dest_desc: ValueId,
    src_ptr: ValueId,
    src_len: ValueId,
) {
    let stat = b.load_typed(stat_addr, IrType::Int(IntWidth::I32));
    let zero = b.const_i32(0);
    let ok = b.icmp(CmpOp::Eq, stat, zero);
    let copy_bb = b.create_block("alloc_class_star_char_source_copy");
    let done_bb = b.create_block("alloc_class_star_char_source_copy_done");
    b.cond_branch(ok, copy_bb, vec![], done_bb, vec![]);

    b.set_block(copy_bb);
    let dest_base = b.load_typed(dest_desc, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
    b.call(
        FuncRef::External("memcpy".into()),
        vec![dest_base, src_ptr, src_len],
        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
    );
    b.branch(done_bb, vec![]);
    b.set_block(done_bb);
}

fn emit_scalar_fixed_char_source_copy_on_success(
    b: &mut FuncBuilder,
    stat_addr: ValueId,
    dest_desc: ValueId,
    src_ptr: ValueId,
    src_len: ValueId,
) {
    let stat = b.load_typed(stat_addr, IrType::Int(IntWidth::I32));
    let zero = b.const_i32(0);
    let ok = b.icmp(CmpOp::Eq, stat, zero);
    let copy_bb = b.create_block("alloc_fixed_char_source_copy");
    let done_bb = b.create_block("alloc_fixed_char_source_copy_done");
    b.cond_branch(ok, copy_bb, vec![], done_bb, vec![]);

    b.set_block(copy_bb);
    let dest_base = b.load_typed(dest_desc, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
    let dest_len = descriptor_elem_size(b, dest_desc);
    b.call(
        FuncRef::External("afs_assign_char_fixed".into()),
        vec![dest_base, dest_len, src_ptr, src_len],
        IrType::Void,
    );
    b.branch(done_bb, vec![]);
    b.set_block(done_bb);
}

fn finalize_assignment_lhs(b: &mut FuncBuilder, ctx: &LowerCtx, type_name: &str, dest: ValueId) {
    if let Some(layout) = ctx.type_layouts.get(type_name) {
        finalize_derived_storage(
            b,
            ctx.st,
            ctx.internal_funcs,
            Some(ctx.contained_host_refs),
            &ctx.locals,
            ctx.type_layouts,
            layout,
            dest,
        );
    }
}

fn prepare_descriptor_assignment_lhs(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    desc: ValueId,
    is_polymorphic: bool,
    layout: &crate::sema::type_layout::TypeLayout,
    stat_addr: ValueId,
) {
    if is_polymorphic {
        require_context_free_dynamic_lifecycle(b, desc);
    }
    finalize_derived_descriptor_storage_if_allocated(
        b,
        ctx.st,
        ctx.internal_funcs,
        Some(ctx.contained_host_refs),
        &ctx.locals,
        desc,
        layout,
        ctx.type_layouts,
    );
    deallocate_derived_descriptor_components(b, desc, layout, ctx.type_layouts, stat_addr);
}

fn stabilize_finalized_assignment_rhs(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    type_name: &str,
    rhs: ValueId,
) -> ValueId {
    if ctx
        .type_layouts
        .get(type_name)
        .is_some_and(|layout| derived_layout_needs_finalization(layout, ctx.type_layouts))
    {
        stabilize_derived_call_result(b, ctx.type_layouts, type_name, rhs)
    } else {
        rhs
    }
}

fn finalizable_function_result_type_name(ctx: &LowerCtx, expr: &SpannedExpr) -> Option<String> {
    let Expr::FunctionCall { callee, .. } = &expr.node else {
        return None;
    };
    let Expr::Name { name } = &callee.node else {
        return None;
    };
    let key = name.to_lowercase();
    let abi_lookup_keys = procedure_abi_lookup_keys_for_call_target(ctx.st, name.as_str(), &[&key]);
    let hidden_abi =
        first_procedure_lookup(&abi_lookup_keys, |k| callee_hidden_result_abi(ctx.st, k))?;
    if hidden_abi != HiddenResultAbi::DerivedAggregate {
        return None;
    }
    let type_name = first_procedure_lookup(&abi_lookup_keys, |k| {
        callee_return_stabilized_derived_type_name(ctx.st, k)
    })?;
    ctx.type_layouts
        .get(&type_name)
        .filter(|layout| derived_layout_needs_finalization(layout, ctx.type_layouts))
        .map(|layout| layout.name.clone())
}

pub(crate) fn lower_stmts(b: &mut FuncBuilder, ctx: &mut LowerCtx, stmts: &[SpannedStmt]) {
    for stmt in stmts {
        // Labeled statements and labeled CONTINUEs create new basic blocks; they must be
        // processed even after a branch/goto terminates the current block. All other dead
        // code (statements after a terminator in an unlabeled position) is skipped.
        let is_label_creating = matches!(
            &stmt.node,
            Stmt::Labeled { .. } | Stmt::Continue { label: Some(_) }
        );
        if !is_label_creating && b.func().block(b.current_block()).terminator.is_some() {
            continue; // dead code — but keep looping so we can find the next label
        }
        lower_stmt(b, ctx, stmt);
    }
}

fn goto_exits_active_block(ctx: &LowerCtx<'_>, label: u64) -> bool {
    ctx.block_cleanups
        .last()
        .is_some_and(|scope| !scope.labels.contains(&label))
}

fn emit_block_cleanups_for_goto(b: &mut FuncBuilder, ctx: &LowerCtx<'_>, label: u64) {
    for scope in ctx.block_cleanups.iter().rev() {
        if scope.labels.contains(&label) {
            break;
        }
        insert_implicit_dealloc(
            b,
            &scope.owned_locals,
            &ctx.locals,
            ctx.type_layouts,
            ctx.st,
            ctx.internal_funcs,
            Some(ctx.contained_host_refs),
            None,
            true,
        );
    }
}

fn copy_array_result_to_fixed_dest(
    b: &mut FuncBuilder,
    info: &LocalInfo,
    src_desc: ValueId,
    src_elem_ty: Option<&IrType>,
) {
    let n = array_total_elems_value(b, info);
    let elem_bytes = b.const_i64(ir_scalar_byte_size(&info.ty, b.layout));
    let byte_count = b.imul(n, elem_bytes);
    let src_kind_tag = src_elem_ty.and_then(numeric_kind_tag_for_ir_type);
    let dest_kind_tag = numeric_kind_tag_for_ir_type(&info.ty);
    if let (Some(sk), Some(dk)) = (src_kind_tag, dest_kind_tag) {
        if sk != dk {
            let dk_v = b.const_i32(dk);
            let sk_v = b.const_i32(sk);
            b.call(
                FuncRef::External("afs_copy_array_result_to_fixed_convert".into()),
                vec![info.addr, src_desc, byte_count, dk_v, sk_v],
                IrType::Void,
            );
            return;
        }
    }
    b.call(
        FuncRef::External("afs_copy_array_result_to_fixed".into()),
        vec![info.addr, src_desc, byte_count],
        IrType::Void,
    );
}

fn copy_array_result_to_descriptor_dest(b: &mut FuncBuilder, info: &LocalInfo, src_desc: ValueId) {
    let dest_desc = array_descriptor_addr(b, info);
    let null_stat = b.const_i64(0);
    b.call(
        FuncRef::External("afs_copy_array_data_no_realloc".into()),
        vec![dest_desc, src_desc, null_stat],
        IrType::Void,
    );
}

struct WhereSectionTemp {
    name: String,
    desc: ValueId,
}

enum WhereMaskValue {
    Scalar(ValueId),
    Array {
        desc: ValueId,
        elem_ty: IrType,
        rank: usize,
    },
}

fn lower_where_mask_value(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    expr: &SpannedExpr,
) -> WhereMaskValue {
    let rank = actual_expr_rank(expr, &ctx.locals, ctx.st, Some(ctx.type_layouts)).unwrap_or(0);
    if rank > 0 {
        if let Some((source_desc, elem_ty)) = lower_array_expr_descriptor(
            b,
            &ctx.locals,
            expr,
            ctx.st,
            Some(ctx.type_layouts),
            Some(ctx.internal_funcs),
            Some(ctx.contained_host_refs),
            Some(ctx.descriptor_params),
        ) {
            let tmp_desc =
                allocate_like_array_temp_descriptor_with_elem_type(b, source_desc, &elem_ty);
            let stat = b.alloca(IrType::Int(IntWidth::I32));
            let zero = b.const_i32(0);
            b.store(zero, stat);
            b.call(
                FuncRef::External("afs_copy_array_data".into()),
                vec![tmp_desc, source_desc, stat],
                IrType::Void,
            );
            deallocate_array_expr_descriptor_if_temp(b, &ctx.locals, expr, ctx.st, source_desc);
            return WhereMaskValue::Array {
                desc: tmp_desc,
                elem_ty,
                rank,
            };
        }
    }

    let raw = super::expr::lower_expr_ctx_tl(b, ctx, expr);
    WhereMaskValue::Scalar(coerce_to_type(b, raw, &IrType::Bool))
}

fn where_mask_value_at(b: &mut FuncBuilder, mask: &WhereMaskValue, index: ValueId) -> ValueId {
    match mask {
        WhereMaskValue::Scalar(value) => *value,
        WhereMaskValue::Array {
            desc,
            elem_ty,
            rank,
        } => {
            let raw = load_array_desc_elem_rank(b, *desc, elem_ty, index, *rank);
            coerce_to_type(b, raw, &IrType::Bool)
        }
    }
}

fn finish_where_mask_values(b: &mut FuncBuilder, masks: &[&WhereMaskValue]) {
    for mask in masks {
        if let WhereMaskValue::Array { desc, .. } = mask {
            deallocate_array_temp_descriptor(b, *desc);
        }
    }
}

fn where_pending_mask_at(
    b: &mut FuncBuilder,
    index: ValueId,
    main_mask: &WhereMaskValue,
    elsewhere_masks: &[WhereMaskValue],
) -> ValueId {
    let main = where_mask_value_at(b, main_mask, index);
    let mut pending = b.not(main);
    for mask in elsewhere_masks {
        let masked = where_mask_value_at(b, mask, index);
        let not_mask = b.not(masked);
        pending = b.and(pending, not_mask);
    }
    pending
}

fn lower_where_array_pass<F>(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    array_names: &[String],
    array_bases: &HashMap<String, ValueId>,
    n: ValueId,
    body: &[SpannedStmt],
    mut cond_for_index: F,
) where
    F: FnMut(&mut FuncBuilder, &mut LowerCtx, ValueId) -> ValueId,
{
    let rewritten_body: Vec<SpannedStmt> = body
        .iter()
        .map(|s| rewrite_scalarized_section_refs_stmt(s, array_names))
        .collect();

    let i_addr = b.alloca(IrType::Int(IntWidth::I64));
    let i_zero = b.const_i64(0);
    b.store(i_zero, i_addr);

    let bb_check = b.create_block("where_check");
    let bb_body = b.create_block("where_body");
    let bb_then = b.create_block("where_then");
    let bb_incr = b.create_block("where_incr");
    let bb_exit = b.create_block("where_exit");
    b.branch(bb_check, vec![]);

    b.set_block(bb_check);
    let i = b.load(i_addr);
    let done = b.icmp(CmpOp::Ge, i, n);
    b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

    b.set_block(bb_body);
    let i_val = b.load(i_addr);
    let mut saved_locals: Vec<(String, Option<LocalInfo>)> = Vec::new();
    for arr_name in array_names {
        saved_locals.push((arr_name.clone(), ctx.locals.get(arr_name).cloned()));
        if let Some(orig_info) = ctx.locals.get(arr_name).cloned() {
            let base = *array_bases.get(arr_name).unwrap();
            let elem_bytes_val = array_elem_size_value(b, &orig_info);
            let byte_off = b.imul(i_val, elem_bytes_val);
            let elem_ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
            ctx.locals.insert(
                arr_name.clone(),
                LocalInfo {
                    addr: elem_ptr,
                    ty: orig_info.ty.clone(),
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
    }

    let cond = cond_for_index(b, ctx, i_val);
    b.cond_branch(cond, bb_then, vec![], bb_incr, vec![]);

    b.set_block(bb_then);
    lower_stmts(b, ctx, &rewritten_body);
    if b.func().block(b.current_block()).terminator.is_none() {
        b.branch(bb_incr, vec![]);
    }

    b.set_block(bb_incr);
    for (name, orig) in saved_locals {
        if let Some(info) = orig {
            ctx.locals.insert(name, info);
        } else {
            ctx.locals.remove(&name);
        }
    }

    let i_cur = b.load(i_addr);
    let one = b.const_i64(1);
    let next = b.iadd(i_cur, one);
    b.store(next, i_addr);
    b.branch(bb_check, vec![]);

    b.set_block(bb_exit);
}

fn lower_where_array_if_else_pass<F>(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    array_names: &[String],
    array_bases: &HashMap<String, ValueId>,
    n: ValueId,
    then_body: &[SpannedStmt],
    else_body: &[SpannedStmt],
    mut cond_for_index: F,
) where
    F: FnMut(&mut FuncBuilder, &mut LowerCtx, ValueId) -> ValueId,
{
    let rewritten_then: Vec<SpannedStmt> = then_body
        .iter()
        .map(|s| rewrite_scalarized_section_refs_stmt(s, array_names))
        .collect();
    let rewritten_else: Vec<SpannedStmt> = else_body
        .iter()
        .map(|s| rewrite_scalarized_section_refs_stmt(s, array_names))
        .collect();

    let i_addr = b.alloca(IrType::Int(IntWidth::I64));
    let i_zero = b.const_i64(0);
    b.store(i_zero, i_addr);

    let bb_check = b.create_block("where_check");
    let bb_body = b.create_block("where_body");
    let bb_then = b.create_block("where_then");
    let bb_else = b.create_block("where_else");
    let bb_incr = b.create_block("where_incr");
    let bb_exit = b.create_block("where_exit");
    b.branch(bb_check, vec![]);

    b.set_block(bb_check);
    let i = b.load(i_addr);
    let done = b.icmp(CmpOp::Ge, i, n);
    b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

    b.set_block(bb_body);
    let i_val = b.load(i_addr);
    let mut saved_locals: Vec<(String, Option<LocalInfo>)> = Vec::new();
    for arr_name in array_names {
        saved_locals.push((arr_name.clone(), ctx.locals.get(arr_name).cloned()));
        if let Some(orig_info) = ctx.locals.get(arr_name).cloned() {
            let base = *array_bases.get(arr_name).unwrap();
            let elem_bytes_val = array_elem_size_value(b, &orig_info);
            let byte_off = b.imul(i_val, elem_bytes_val);
            let elem_ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
            ctx.locals.insert(
                arr_name.clone(),
                LocalInfo {
                    addr: elem_ptr,
                    ty: orig_info.ty.clone(),
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
    }

    let cond = cond_for_index(b, ctx, i_val);
    b.cond_branch(cond, bb_then, vec![], bb_else, vec![]);

    b.set_block(bb_then);
    lower_stmts(b, ctx, &rewritten_then);
    if b.func().block(b.current_block()).terminator.is_none() {
        b.branch(bb_incr, vec![]);
    }

    b.set_block(bb_else);
    lower_stmts(b, ctx, &rewritten_else);
    if b.func().block(b.current_block()).terminator.is_none() {
        b.branch(bb_incr, vec![]);
    }

    b.set_block(bb_incr);
    for (name, orig) in saved_locals {
        if let Some(info) = orig {
            ctx.locals.insert(name, info);
        } else {
            ctx.locals.remove(&name);
        }
    }

    let i_cur = b.load(i_addr);
    let one = b.const_i64(1);
    let next = b.iadd(i_cur, one);
    b.store(next, i_addr);
    b.branch(bb_check, vec![]);

    b.set_block(bb_exit);
}

fn simple_array_element_designator(
    expr: &SpannedExpr,
) -> Option<(String, Vec<Argument>, crate::lexer::Span)> {
    let Expr::FunctionCall { callee, args } = &expr.node else {
        return None;
    };
    let Expr::Name { name } = &callee.node else {
        return None;
    };
    if args
        .iter()
        .any(|arg| !matches!(arg.value, SectionSubscript::Element(_)))
    {
        return None;
    }
    Some((name.to_lowercase(), args.clone(), expr.span))
}

fn synth_array_element_expr(
    array_name: &str,
    args: &[Argument],
    span: crate::lexer::Span,
) -> SpannedExpr {
    Spanned::new(
        Expr::FunctionCall {
            callee: Box::new(synth_name_expr(array_name, span)),
            args: args.to_vec(),
        },
        span,
    )
}

fn forall_temp_supported_element(info: &LocalInfo) -> bool {
    info.char_kind == CharKind::None && info.derived_type.is_none()
}

fn insert_forall_temp_local(
    ctx: &mut LowerCtx,
    name: String,
    desc: ValueId,
    ty: IrType,
    rank: usize,
    logical_kind: Option<u8>,
) {
    ctx.locals.insert(
        name,
        LocalInfo {
            addr: desc,
            ty,
            dims: vec![(1, 0); rank],
            allocatable: false,
            descriptor_arg: true,
            by_ref: false,
            char_kind: CharKind::None,
            derived_type: None,
            inline_const: None,
            is_pointer: false,
            runtime_dim_upper: vec![],
            is_class: false,
            logical_kind,
            last_dim_assumed_size: false,
        },
    );
}

fn try_lower_forall_assignment_with_temp(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    specs: &[ForallSpec],
    mask: Option<&SpannedExpr>,
    body: &[SpannedStmt],
) -> bool {
    if specs.is_empty() || body.len() != 1 {
        return false;
    }

    let Stmt::Assignment { target, value } = &body[0].node else {
        return false;
    };
    let Some((target_name, target_args, target_span)) = simple_array_element_designator(target)
    else {
        return false;
    };
    let Some(target_info) = ctx.locals.get(&target_name).cloned() else {
        return false;
    };
    if !local_is_array_like(&target_info) || !forall_temp_supported_element(&target_info) {
        return false;
    }
    let rank = local_declared_rank(&target_info).max(target_args.len());
    if rank == 0 || target_args.len() != rank {
        return false;
    }

    let source_desc = if local_uses_array_descriptor(&target_info) {
        array_descriptor_addr(b, &target_info)
    } else {
        materialize_array_descriptor_for_info(b, &target_info)
    };
    let value_desc =
        allocate_like_array_temp_descriptor_with_elem_type(b, source_desc, &target_info.ty);
    let value_temp_name = fresh_elemental_temp_name(&ctx.locals, "afs_forall_value", 0);
    insert_forall_temp_local(
        ctx,
        value_temp_name.clone(),
        value_desc,
        target_info.ty.clone(),
        rank,
        target_info.logical_kind,
    );

    let mut temps_to_remove = vec![value_temp_name.clone()];
    let mut temps_to_deallocate = vec![value_desc];

    let value_temp_target = synth_array_element_expr(&value_temp_name, &target_args, target_span);
    let fill_value_stmt = Spanned::new(
        Stmt::Assignment {
            target: value_temp_target.clone(),
            value: value.clone(),
        },
        body[0].span,
    );

    if let Some(mask_expr) = mask {
        let mask_desc =
            allocate_like_array_temp_descriptor_with_elem_type(b, source_desc, &IrType::Bool);
        let mask_temp_name = fresh_elemental_temp_name(&ctx.locals, "afs_forall_active", 1);
        insert_forall_temp_local(
            ctx,
            mask_temp_name.clone(),
            mask_desc,
            IrType::Bool,
            rank,
            None,
        );
        temps_to_remove.push(mask_temp_name.clone());
        temps_to_deallocate.push(mask_desc);

        let mask_temp_target = synth_array_element_expr(&mask_temp_name, &target_args, target_span);
        let save_mask_stmt = Spanned::new(
            Stmt::Assignment {
                target: mask_temp_target.clone(),
                value: mask_expr.clone(),
            },
            mask_expr.span,
        );
        let fill_guard_stmt = Spanned::new(
            Stmt::IfConstruct {
                name: None,
                condition: mask_expr.clone(),
                then_body: vec![fill_value_stmt],
                else_ifs: vec![],
                else_body: None,
            },
            mask_expr.span,
        );
        let first_pass = vec![save_mask_stmt, fill_guard_stmt];
        lower_forall_nested(b, ctx, specs, None, &first_pass);

        let replay_value = synth_array_element_expr(&value_temp_name, &target_args, target_span);
        let replay_assignment = Spanned::new(
            Stmt::Assignment {
                target: target.clone(),
                value: replay_value,
            },
            body[0].span,
        );
        let replay_guard = Spanned::new(
            Stmt::IfConstruct {
                name: None,
                condition: mask_temp_target,
                then_body: vec![replay_assignment],
                else_ifs: vec![],
                else_body: None,
            },
            mask_expr.span,
        );
        let second_pass = vec![replay_guard];
        lower_forall_nested(b, ctx, specs, None, &second_pass);
    } else {
        let first_pass = vec![fill_value_stmt];
        lower_forall_nested(b, ctx, specs, None, &first_pass);

        let replay_value = synth_array_element_expr(&value_temp_name, &target_args, target_span);
        let replay_assignment = Spanned::new(
            Stmt::Assignment {
                target: target.clone(),
                value: replay_value,
            },
            body[0].span,
        );
        let second_pass = vec![replay_assignment];
        lower_forall_nested(b, ctx, specs, None, &second_pass);
    }

    for desc in temps_to_deallocate {
        deallocate_array_temp_descriptor(b, desc);
    }
    for name in temps_to_remove {
        ctx.locals.remove(&name);
    }
    true
}

fn lower_where_section_temp(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    expr: &SpannedExpr,
    next_temp: &mut usize,
    temps: &mut Vec<WhereSectionTemp>,
) -> Option<SpannedExpr> {
    let Expr::FunctionCall { callee, args } = &expr.node else {
        return None;
    };
    let Expr::Name { name } = &callee.node else {
        return None;
    };
    if !args
        .iter()
        .any(|a| matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. }))
    {
        return None;
    }
    let key = name.to_lowercase();
    if !ctx.locals.get(&key).is_some_and(local_is_array_like) {
        return None;
    }

    let rank = actual_expr_rank(expr, &ctx.locals, ctx.st, Some(ctx.type_layouts))
        .unwrap_or(1)
        .max(1);
    let (source_desc, elem_ty) = lower_array_expr_descriptor(
        b,
        &ctx.locals,
        expr,
        ctx.st,
        Some(ctx.type_layouts),
        Some(ctx.internal_funcs),
        Some(ctx.contained_host_refs),
        Some(ctx.descriptor_params),
    )?;
    let tmp_desc = allocate_like_array_temp_descriptor_with_elem_type(b, source_desc, &elem_ty);
    let stat = b.alloca(IrType::Int(IntWidth::I32));
    let zero = b.const_i32(0);
    b.store(zero, stat);
    b.call(
        FuncRef::External("afs_copy_array_data".into()),
        vec![tmp_desc, source_desc, stat],
        IrType::Void,
    );
    deallocate_array_expr_descriptor_if_temp(b, &ctx.locals, expr, ctx.st, source_desc);

    let temp_name = fresh_elemental_temp_name(&ctx.locals, "afs_where_section", *next_temp);
    *next_temp += 1;
    ctx.locals.insert(
        temp_name.clone(),
        LocalInfo {
            addr: tmp_desc,
            ty: elem_ty,
            dims: vec![(1, 0); rank],
            allocatable: false,
            descriptor_arg: true,
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
    temps.push(WhereSectionTemp {
        name: temp_name.clone(),
        desc: tmp_desc,
    });
    Some(synth_name_expr(&temp_name, expr.span))
}

fn rewrite_where_read_sections_to_temps(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    expr: &SpannedExpr,
    next_temp: &mut usize,
    temps: &mut Vec<WhereSectionTemp>,
) -> SpannedExpr {
    use crate::ast::Spanned;

    if let Some(temp) = lower_where_section_temp(b, ctx, expr, next_temp, temps) {
        return temp;
    }

    match &expr.node {
        Expr::FunctionCall { callee, args } => {
            let new_callee = Box::new(rewrite_where_read_sections_to_temps(
                b, ctx, callee, next_temp, temps,
            ));
            let new_args = args
                .iter()
                .map(|a| crate::ast::expr::Argument {
                    keyword: a.keyword.clone(),
                    value: match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => {
                            crate::ast::expr::SectionSubscript::Element(
                                rewrite_where_read_sections_to_temps(b, ctx, e, next_temp, temps),
                            )
                        }
                        crate::ast::expr::SectionSubscript::Range { start, end, stride } => {
                            crate::ast::expr::SectionSubscript::Range {
                                start: start.as_ref().map(|e| {
                                    rewrite_where_read_sections_to_temps(
                                        b, ctx, e, next_temp, temps,
                                    )
                                }),
                                end: end.as_ref().map(|e| {
                                    rewrite_where_read_sections_to_temps(
                                        b, ctx, e, next_temp, temps,
                                    )
                                }),
                                stride: stride.as_ref().map(|e| {
                                    rewrite_where_read_sections_to_temps(
                                        b, ctx, e, next_temp, temps,
                                    )
                                }),
                            }
                        }
                    },
                })
                .collect();
            Spanned::new(
                Expr::FunctionCall {
                    callee: new_callee,
                    args: new_args,
                },
                expr.span,
            )
        }
        Expr::BinaryOp { op, left, right } => Spanned::new(
            Expr::BinaryOp {
                op: op.clone(),
                left: Box::new(rewrite_where_read_sections_to_temps(
                    b, ctx, left, next_temp, temps,
                )),
                right: Box::new(rewrite_where_read_sections_to_temps(
                    b, ctx, right, next_temp, temps,
                )),
            },
            expr.span,
        ),
        Expr::UnaryOp { op, operand } => Spanned::new(
            Expr::UnaryOp {
                op: op.clone(),
                operand: Box::new(rewrite_where_read_sections_to_temps(
                    b, ctx, operand, next_temp, temps,
                )),
            },
            expr.span,
        ),
        Expr::ParenExpr { inner } => Spanned::new(
            Expr::ParenExpr {
                inner: Box::new(rewrite_where_read_sections_to_temps(
                    b, ctx, inner, next_temp, temps,
                )),
            },
            expr.span,
        ),
        Expr::ComponentAccess { base, component } => Spanned::new(
            Expr::ComponentAccess {
                base: Box::new(rewrite_where_read_sections_to_temps(
                    b, ctx, base, next_temp, temps,
                )),
                component: component.clone(),
            },
            expr.span,
        ),
        Expr::ArrayConstructor { values, type_spec } => {
            let new_values = values
                .iter()
                .map(|value| match value {
                    crate::ast::expr::AcValue::Expr(e) => crate::ast::expr::AcValue::Expr(
                        rewrite_where_read_sections_to_temps(b, ctx, e, next_temp, temps),
                    ),
                    crate::ast::expr::AcValue::ImpliedDo(ido) => {
                        crate::ast::expr::AcValue::ImpliedDo(Box::new(
                            crate::ast::expr::ImpliedDoLoop {
                                values: ido
                                    .values
                                    .iter()
                                    .map(|inner| match inner {
                                        crate::ast::expr::AcValue::Expr(e) => {
                                            crate::ast::expr::AcValue::Expr(
                                                rewrite_where_read_sections_to_temps(
                                                    b, ctx, e, next_temp, temps,
                                                ),
                                            )
                                        }
                                        crate::ast::expr::AcValue::ImpliedDo(nested) => {
                                            crate::ast::expr::AcValue::ImpliedDo(nested.clone())
                                        }
                                    })
                                    .collect(),
                                var: ido.var.clone(),
                                start: rewrite_where_read_sections_to_temps(
                                    b, ctx, &ido.start, next_temp, temps,
                                ),
                                end: rewrite_where_read_sections_to_temps(
                                    b, ctx, &ido.end, next_temp, temps,
                                ),
                                step: ido.step.as_ref().map(|e| {
                                    rewrite_where_read_sections_to_temps(
                                        b, ctx, e, next_temp, temps,
                                    )
                                }),
                            },
                        ))
                    }
                })
                .collect();
            Spanned::new(
                Expr::ArrayConstructor {
                    values: new_values,
                    type_spec: type_spec.clone(),
                },
                expr.span,
            )
        }
        _ => expr.clone(),
    }
}

fn rewrite_where_read_sections_to_temps_stmt(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    stmt: &SpannedStmt,
    next_temp: &mut usize,
    temps: &mut Vec<WhereSectionTemp>,
) -> SpannedStmt {
    use crate::ast::Spanned;
    match &stmt.node {
        Stmt::Assignment { target, value } => Spanned::new(
            Stmt::Assignment {
                target: target.clone(),
                value: rewrite_where_read_sections_to_temps(b, ctx, value, next_temp, temps),
            },
            stmt.span,
        ),
        _ => stmt.clone(),
    }
}

fn finish_where_section_temps(b: &mut FuncBuilder, ctx: &mut LowerCtx, temps: &[WhereSectionTemp]) {
    for temp in temps {
        deallocate_array_temp_descriptor(b, temp.desc);
        ctx.locals.remove(&temp.name);
    }
}

fn synth_defined_unary_array_result_call(
    ctx: &LowerCtx<'_>,
    value: &crate::ast::expr::SpannedExpr,
) -> Option<crate::ast::expr::SpannedExpr> {
    let Expr::UnaryOp { op, operand } = &value.node else {
        return None;
    };
    let specific = resolve_defined_unary_operator_specific_by_semantics(
        ctx.st,
        Some(&ctx.locals),
        Some(ctx.type_layouts),
        op,
        operand,
    )?;
    let specific_key = specific.to_lowercase();
    let (call_name, callee_key) = resolved_symbol_call_target(ctx.st, &specific_key, &specific);
    let abi_lookup_keys = procedure_abi_lookup_keys_for_call_target(
        ctx.st,
        call_name.as_str(),
        &[&callee_key, &specific_key],
    );
    if !matches!(
        first_procedure_lookup(&abi_lookup_keys, |k| callee_hidden_result_abi(ctx.st, k)),
        Some(HiddenResultAbi::ArrayDescriptor)
    ) {
        return None;
    }

    Some(crate::ast::Spanned::new(
        Expr::FunctionCall {
            callee: Box::new(crate::ast::Spanned::new(
                Expr::Name { name: specific },
                operand.span,
            )),
            args: vec![crate::ast::expr::Argument {
                keyword: None,
                value: crate::ast::expr::SectionSubscript::Element((**operand).clone()),
            }],
        },
        value.span,
    ))
}

fn io_control_by_keyword<'a>(controls: &'a [IoControl], needle: &str) -> Option<&'a IoControl> {
    controls.iter().find(|c| {
        c.keyword
            .as_deref()
            .map(|k| k.eq_ignore_ascii_case(needle))
            .unwrap_or(false)
    })
}

fn namelist_i8_ptr(b: &mut FuncBuilder, value: ValueId) -> ValueId {
    if b.func()
        .value_type(value)
        .is_some_and(|ty| ty == IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
    {
        return value;
    }
    let raw = b.ptr_to_int(value);
    b.int_to_ptr(raw, IrType::Int(IntWidth::I8))
}

fn namelist_group_name(ctrl: &IoControl) -> Option<String> {
    fn from_expr(expr: &SpannedExpr) -> Option<String> {
        match &expr.node {
            Expr::Name { name } => Some(name.clone()),
            Expr::ParenExpr { inner } => from_expr(inner),
            _ => None,
        }
    }
    from_expr(&ctrl.value)
}

fn namelist_unit_control(controls: &[IoControl]) -> Option<&IoControl> {
    controls.iter().find(|c| {
        c.keyword
            .as_deref()
            .map(|k| k.eq_ignore_ascii_case("unit"))
            .unwrap_or(true)
    })
}

fn lower_namelist_unit(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    controls: &[IoControl],
    star_unit: i32,
) -> ValueId {
    if let Some(ctrl) = namelist_unit_control(controls) {
        if matches!(&ctrl.value.node, Expr::Name { name } if name == "*") {
            b.const_i32(star_unit)
        } else {
            super::expr::lower_expr_ctx(b, ctx, &ctrl.value)
        }
    } else {
        b.const_i32(star_unit)
    }
}

fn lower_namelist_entry_value(
    b: &mut FuncBuilder,
    info: &LocalInfo,
    is_logical: bool,
) -> Option<(ValueId, i32, ValueId, ValueId)> {
    let one = b.const_i64(1);
    if matches!(info.char_kind, CharKind::Deferred) && info.dims.is_empty() && !info.is_pointer {
        let desc = string_descriptor_addr(b, info);
        return Some((namelist_i8_ptr(b, desc), 4, b.const_i64(0), one));
    }
    if local_is_array_like(info)
        && (info.char_kind != CharKind::None || descriptor_backed_runtime_char_array(info))
    {
        let raw_base = array_base_addr(b, info);
        let base = namelist_i8_ptr(b, raw_base);
        let elem_count = array_total_elems_value(b, info);
        let elem_len = match info.char_kind {
            CharKind::Fixed(len) => b.const_i64(len),
            CharKind::FixedRuntime { len_addr } | CharKind::AssumedLen { len_addr } => {
                b.load(len_addr)
            }
            _ => array_elem_size_value(b, info),
        };
        return Some((base, 2, elem_len, elem_count));
    }
    if let Some((ptr, len)) = local_char_ptr_and_len(b, info) {
        return Some((namelist_i8_ptr(b, ptr), 2, len, one));
    }

    let data_addr = if local_is_array_like(info) && local_uses_array_descriptor(info) {
        array_base_addr(b, info)
    } else if info.by_ref {
        b.load(info.addr)
    } else {
        info.addr
    };
    let data_ptr = namelist_i8_ptr(b, data_addr);
    let zero_len = b.const_i64(0);
    let elem_count = if local_is_array_like(info) {
        array_total_elems_value(b, info)
    } else {
        one
    };

    if is_logical || info.logical_kind.is_some() {
        match info.ty {
            IrType::Int(IntWidth::I32) => return Some((data_ptr, 3, zero_len, elem_count)),
            IrType::Bool => return Some((data_ptr, 5, zero_len, elem_count)),
            _ => {}
        }
        return None;
    }

    match info.ty {
        IrType::Int(IntWidth::I32) => Some((data_ptr, 0, zero_len, elem_count)),
        IrType::Float(FloatWidth::F64) => Some((data_ptr, 1, zero_len, elem_count)),
        _ => None,
    }
}

#[derive(Clone)]
enum NamelistEntrySource {
    Local {
        name: String,
    },
    Component {
        entry_name: String,
        base_name: String,
        component: String,
        span: crate::lexer::Span,
        is_logical: bool,
    },
}

impl NamelistEntrySource {
    fn entry_name(&self) -> &str {
        match self {
            NamelistEntrySource::Local { name } => name,
            NamelistEntrySource::Component { entry_name, .. } => entry_name,
        }
    }
}

fn expand_namelist_entry_sources(
    ctx: &LowerCtx<'_>,
    vars: &[String],
    span: crate::lexer::Span,
) -> Vec<NamelistEntrySource> {
    let mut sources = Vec::new();
    for var_name in vars {
        let var_key = var_name.to_lowercase();
        if let Some(info) = ctx.locals.get(&var_key) {
            if info.dims.is_empty() && !info.allocatable && !info.is_pointer {
                if let Some(type_name) = info.derived_type.as_deref() {
                    if let Some(layout) = ctx.type_layouts.get(type_name) {
                        sources.extend(layout.fields.iter().map(|field| {
                            NamelistEntrySource::Component {
                                entry_name: format!("{}%{}", var_name, field.name),
                                base_name: var_name.clone(),
                                component: field.name.clone(),
                                span,
                                is_logical: matches!(
                                    field.type_info,
                                    crate::sema::symtab::TypeInfo::Logical { .. }
                                ),
                            }
                        }));
                        continue;
                    }
                }
            }
        }
        sources.push(NamelistEntrySource::Local {
            name: var_name.clone(),
        });
    }
    sources
}

fn namelist_component_expr(
    base_name: &str,
    component: &str,
    span: crate::lexer::Span,
) -> SpannedExpr {
    let base = crate::ast::Spanned::new(
        Expr::Name {
            name: base_name.to_string(),
        },
        span,
    );
    crate::ast::Spanned::new(
        Expr::ComponentAccess {
            base: Box::new(base),
            component: component.to_string(),
        },
        span,
    )
}

fn lower_namelist_entries(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    group_name: &str,
    span: crate::lexer::Span,
) -> (ValueId, ValueId) {
    let key = group_name.to_lowercase();
    let Some(sym) = ctx
        .st
        .lookup_local_then_any(ctx.proc_scope_id, &key)
        .or_else(|| ctx.st.find_symbol_any_scope(&key))
    else {
        lower_stmt_error(
            span,
            &format!("NAMELIST group '{}' is not declared", group_name),
        );
    };
    if sym.kind != crate::sema::symtab::SymbolKind::Namelist {
        lower_stmt_error(span, &format!("'{}' is not a NAMELIST group", group_name));
    }
    let vars = sym.arg_names.clone();
    let sources = expand_namelist_entry_sources(ctx, &vars, span);
    let entry_size = 48_i64;
    let entries = b.alloca(IrType::Array(
        Box::new(IrType::Int(IntWidth::I8)),
        entry_size as u64 * sources.len() as u64,
    ));
    let ptr_ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)));
    for (idx, source) in sources.iter().enumerate() {
        let entry_name = source.entry_name();
        let (info, is_logical) = match source {
            NamelistEntrySource::Local { name } => {
                let var_key = name.to_lowercase();
                let Some(info) = ctx.locals.get(&var_key).cloned() else {
                    lower_stmt_error(
                        span,
                        &format!(
                            "NAMELIST variable '{}' is not available in this scope",
                            name
                        ),
                    );
                };
                let is_logical = ctx
                    .st
                    .lookup_local_then_any(ctx.proc_scope_id, &var_key)
                    .and_then(|sym| sym.type_info.as_ref())
                    .is_some_and(|ty| matches!(ty, crate::sema::symtab::TypeInfo::Logical { .. }));
                (info, is_logical)
            }
            NamelistEntrySource::Component {
                base_name,
                component,
                span,
                is_logical,
                ..
            } => {
                let expr = namelist_component_expr(base_name, component, *span);
                let Some(info) =
                    component_field_local_info(b, &ctx.locals, &expr, ctx.st, ctx.type_layouts)
                else {
                    lower_stmt_error(
                        *span,
                        &format!(
                            "NAMELIST variable '{}' is not available in this scope",
                            entry_name
                        ),
                    );
                };
                (info, *is_logical)
            }
        };
        let Some((data_ptr, data_type, data_len, elem_count)) =
            lower_namelist_entry_value(b, &info, is_logical)
        else {
            lower_stmt_error(
                span,
                &format!("NAMELIST variable '{}' has unsupported type", entry_name),
            );
        };
        let base = idx as i64 * entry_size;
        let name_ptr = b.const_string(entry_name.as_bytes());
        let name_len = b.const_i64(entry_name.len() as i64);
        let data_type_value = b.const_i32(data_type);
        store_byte_aggregate_field(b, entries, base, ptr_ty.clone(), name_ptr);
        store_byte_aggregate_field(b, entries, base + 8, IrType::Int(IntWidth::I64), name_len);
        store_byte_aggregate_field(b, entries, base + 16, ptr_ty.clone(), data_ptr);
        store_byte_aggregate_field(
            b,
            entries,
            base + 24,
            IrType::Int(IntWidth::I32),
            data_type_value,
        );
        store_byte_aggregate_field(b, entries, base + 32, IrType::Int(IntWidth::I64), data_len);
        store_byte_aggregate_field(
            b,
            entries,
            base + 40,
            IrType::Int(IntWidth::I64),
            elem_count,
        );
    }
    (entries, b.const_i32(sources.len() as i32))
}

fn namelist_internal_io_buffer(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    control: &IoControl,
) -> Option<(ValueId, ValueId)> {
    if control
        .keyword
        .as_deref()
        .map(|k| !k.eq_ignore_ascii_case("unit"))
        .unwrap_or(false)
    {
        return None;
    }
    if let Expr::Name { name } = &control.value.node {
        if let Some(info) = ctx.locals.get(&name.to_lowercase()).cloned() {
            if local_is_array_like(&info)
                && (info.char_kind != CharKind::None || descriptor_backed_runtime_char_array(&info))
            {
                let raw_base = array_base_addr(b, &info);
                let base = namelist_i8_ptr(b, raw_base);
                let elem_len = match info.char_kind {
                    CharKind::Fixed(len) => b.const_i64(len),
                    CharKind::FixedRuntime { len_addr } => b.load(len_addr),
                    _ => array_elem_size_value(b, &info),
                };
                let n = array_total_elems_value(b, &info);
                return Some((base, b.imul(n, elem_len)));
            }
        }
    }
    internal_io_buffer(b, ctx, control)
}

fn lower_namelist_read_stmt(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    controls: &[IoControl],
    iostat_addr: ValueId,
    end_label: Option<u64>,
    err_label: Option<u64>,
    user_iostat: bool,
) -> bool {
    let Some(nml_ctrl) = io_control_by_keyword(controls, "nml") else {
        return false;
    };
    let Some(group_name) = namelist_group_name(nml_ctrl) else {
        lower_stmt_error(nml_ctrl.value.span, "NML= must name a NAMELIST group");
    };
    let (entries, n_entries) = lower_namelist_entries(b, ctx, &group_name, nml_ctrl.value.span);
    let group_ptr = b.const_string(group_name.as_bytes());
    let group_len = b.const_i64(group_name.len() as i64);

    if let Some(ctrl) = controls.first() {
        if let Some((buf_ptr, buf_len)) = namelist_internal_io_buffer(b, ctx, ctrl) {
            b.call(
                FuncRef::External("afs_read_namelist_internal".into()),
                vec![
                    buf_ptr,
                    buf_len,
                    group_ptr,
                    group_len,
                    entries,
                    n_entries,
                    iostat_addr,
                ],
                IrType::Void,
            );
            lower_read_status_branches(b, ctx, end_label, err_label, iostat_addr, user_iostat);
            return true;
        }
    }

    let unit = lower_namelist_unit(b, ctx, controls, 5);
    lower_external_io_pos_seek(b, ctx, controls, unit, iostat_addr);
    b.call(
        FuncRef::External("afs_read_namelist".into()),
        vec![unit, group_ptr, group_len, entries, n_entries, iostat_addr],
        IrType::Void,
    );
    lower_read_status_branches(b, ctx, end_label, err_label, iostat_addr, user_iostat);
    true
}

fn lower_namelist_write_stmt(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    controls: &[IoControl],
    iostat_ptr: ValueId,
) -> bool {
    let Some(nml_ctrl) = io_control_by_keyword(controls, "nml") else {
        return false;
    };
    let Some(group_name) = namelist_group_name(nml_ctrl) else {
        lower_stmt_error(nml_ctrl.value.span, "NML= must name a NAMELIST group");
    };
    if let Some(ctrl) = controls.first() {
        if namelist_internal_io_buffer(b, ctx, ctrl).is_some() {
            lower_stmt_error(
                ctrl.value.span,
                "internal NAMELIST WRITE is not implemented; write to an external unit",
            );
        }
    }
    let (entries, n_entries) = lower_namelist_entries(b, ctx, &group_name, nml_ctrl.value.span);
    let group_ptr = b.const_string(group_name.as_bytes());
    let group_len = b.const_i64(group_name.len() as i64);
    let unit = lower_namelist_unit(b, ctx, controls, 6);
    lower_external_io_pos_seek(b, ctx, controls, unit, iostat_ptr);
    b.call(
        FuncRef::External("afs_write_namelist".into()),
        vec![unit, group_ptr, group_len, entries, n_entries, iostat_ptr],
        IrType::Void,
    );
    true
}

fn static_concrete_expr_type_layout<'a>(
    ctx: &'a LowerCtx<'_>,
    expr: Option<&SpannedExpr>,
) -> Option<&'a crate::sema::type_layout::TypeLayout> {
    let expr = expr?;
    match operator_expr_type_info(expr, Some(&ctx.locals), ctx.st, Some(ctx.type_layouts)) {
        Some(crate::sema::symtab::TypeInfo::Derived(type_name)) => {
            type_layout_for_current_scope(ctx.type_layouts, &type_name)
        }
        _ => None,
    }
}

fn format_label_literal(expr: &SpannedExpr) -> Option<u64> {
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => text.parse::<u64>().ok(),
        Expr::ParenExpr { inner } => format_label_literal(inner),
        _ => None,
    }
}

fn labeled_format_spec<'a>(ctx: &'a LowerCtx<'_>, expr: &SpannedExpr) -> Option<&'a str> {
    format_label_literal(expr)
        .and_then(|label| ctx.format_labels.get(&label))
        .map(String::as_str)
}

fn is_formatted_format_expr(ctx: &LowerCtx<'_>, expr: &SpannedExpr) -> bool {
    if matches!(&expr.node, Expr::Name { name } if name == "*") {
        return false;
    }
    format_label_literal(expr).is_some()
        || crate::sema::types::expr_type(expr, ctx.st).is_character()
}

fn lower_format_expr(
    b: &mut FuncBuilder,
    ctx: &LowerCtx<'_>,
    expr: &SpannedExpr,
) -> (ValueId, ValueId) {
    if let Some(spec) = labeled_format_spec(ctx, expr) {
        let ptr = b.const_string(spec.as_bytes());
        let len = b.const_i64(spec.len() as i64);
        return (ptr, len);
    }
    if let Some(label) = format_label_literal(expr) {
        lower_stmt_error(
            expr.span,
            &format!("FORMAT label {} not defined in this scoping unit", label),
        );
    }
    lower_string_expr_ctx(b, ctx, expr)
}

fn inquire_integer_storeback_type(
    b: &FuncBuilder,
    ctx: &LowerCtx<'_>,
    expr: &crate::ast::expr::SpannedExpr,
    dest_addr: ValueId,
) -> IrType {
    if let Some(ti) =
        operator_expr_type_info(expr, Some(&ctx.locals), ctx.st, Some(ctx.type_layouts))
    {
        let ty = type_info_to_ir_type(&ti);
        if matches!(ty, IrType::Int(_)) {
            return ty;
        }
    }

    match b.func().value_type(dest_addr) {
        Some(IrType::Ptr(inner)) => (*inner).clone(),
        _ => IrType::Int(IntWidth::I32),
    }
}

fn lower_external_io_pos_seek(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    controls: &[IoControl],
    unit: ValueId,
    iostat_ptr: ValueId,
) {
    let Some(pos_ctrl) = io_control_by_keyword(controls, "pos") else {
        return;
    };
    let raw_pos = super::expr::lower_expr_ctx(b, ctx, &pos_ctrl.value);
    let pos = coerce_to_type(b, raw_pos, &IrType::Int(IntWidth::I64));
    let unit_i32 = coerce_to_type(b, unit, &IrType::Int(IntWidth::I32));
    b.call(
        FuncRef::External("afs_seek_stream".into()),
        vec![unit_i32, pos, iostat_ptr],
        IrType::Void,
    );
}

/// Make a conditional-argument arm value match the merge slot type.
/// The only legitimate mismatch is the absent-argument null (i64 0)
/// flowing into a pointer-typed slot, and pointee differences between
/// arms' addresses; both are representation-free casts.
fn conform_condarg_to_slot(b: &mut FuncBuilder, val: ValueId, slot_ty: &IrType) -> ValueId {
    let val_ty = b.func().value_type(val);
    match (&val_ty, slot_ty) {
        (Some(t), s) if t == s => val,
        (Some(IrType::Int(_)), IrType::Ptr(pointee)) => b.int_to_ptr(val, (**pointee).clone()),
        (Some(IrType::Ptr(_)), IrType::Ptr(pointee)) => {
            // Same address, different pointee annotation: round-trip
            // through the integer domain (both casts are free).
            let as_int = b.ptr_to_int(val);
            b.int_to_ptr(as_int, (**pointee).clone())
        }
        _ => val,
    }
}

/// Lower one CALL actual argument, honoring F2023 conditional
/// arguments. A conditional selects between argument ASSOCIATIONS:
/// each arm materializes its own address/descriptor/value in its own
/// block (via `materialize`, the caller's full ABI decision tree) and
/// the merge block carries the chosen one as a block parameter. A
/// `.NIL.` arm produces the same absent representation an omitted
/// OPTIONAL gets (`missing_optional_call_arg`). Never a value temporary:
/// INTENT(OUT)/INOUT writes land in the selected actual. OPTIONAL, VALUE
/// presence is returned separately from the payload.
pub(super) struct MaterializedCallArg {
    pub(super) value: ValueId,
    pub(super) character_len: Option<ValueId>,
    pub(super) owned_character_bases: Vec<ValueId>,
}

impl MaterializedCallArg {
    pub(super) fn plain(value: ValueId) -> Self {
        Self {
            value,
            character_len: None,
            owned_character_bases: Vec::new(),
        }
    }
}

pub(super) struct LoweredCallArg {
    pub(super) value: ValueId,
    pub(super) present: ValueId,
    pub(super) character_len: Option<ValueId>,
    pub(super) owned_character_bases: Vec<ValueId>,
}

impl LoweredCallArg {
    pub(super) fn plain(value: ValueId, present: ValueId) -> Self {
        Self {
            value,
            present,
            character_len: None,
            owned_character_bases: Vec::new(),
        }
    }

    fn materialized(materialized: MaterializedCallArg, present: ValueId) -> Self {
        Self {
            value: materialized.value,
            present,
            character_len: materialized.character_len,
            owned_character_bases: materialized.owned_character_bases,
        }
    }
}

fn null_character_owner(b: &mut FuncBuilder) -> ValueId {
    let zero = b.const_i64(0);
    b.int_to_ptr(zero, IrType::Int(IntWidth::I8))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_call_arg_maybe_conditional(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    st: &crate::sema::symtab::SymbolTable,
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
    internal_funcs: Option<&HashMap<String, u32>>,
    contained_host_refs: Option<&HashMap<String, Vec<String>>>,
    descriptor_params: Option<&HashMap<String, Vec<bool>>>,
    e: &crate::ast::expr::SpannedExpr,
    callee_key: &str,
    arg_index: usize,
    is_value: bool,
    materialize: &mut dyn FnMut(
        &mut FuncBuilder,
        &crate::ast::expr::SpannedExpr,
    ) -> MaterializedCallArg,
) -> LoweredCallArg {
    use crate::ast::expr::Expr;
    match &e.node {
        Expr::NilArgument => {
            let value = missing_optional_call_arg(b, st, callee_key, arg_index, is_value);
            let present = b.const_bool(false);
            LoweredCallArg::plain(value, present)
        }
        Expr::ConditionalExpr {
            cond,
            then_val,
            else_val,
        } => {
            if let Expr::LogicalLiteral { value, .. } = &cond.node {
                let arm = if *value { then_val } else { else_val };
                return lower_call_arg_maybe_conditional(
                    b,
                    locals,
                    st,
                    type_layouts,
                    internal_funcs,
                    contained_host_refs,
                    descriptor_params,
                    arm,
                    callee_key,
                    arg_index,
                    is_value,
                    materialize,
                );
            }
            let cond_raw = super::expr::lower_expr_full(
                b,
                locals,
                cond,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let cond_val = coerce_to_type(b, cond_raw, &IrType::Bool);
            let bb_then = b.create_block("condarg_then");
            let bb_else = b.create_block("condarg_else");
            let bb_merge = b.create_block("condarg_merge");
            b.cond_branch(cond_val, bb_then, vec![], bb_else, vec![]);

            b.set_block(bb_then);
            let t_arg = lower_call_arg_maybe_conditional(
                b,
                locals,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
                then_val,
                callee_key,
                arg_index,
                is_value,
                materialize,
            );
            let bb_then_end = b.current_block();

            b.set_block(bb_else);
            let e_arg = lower_call_arg_maybe_conditional(
                b,
                locals,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
                else_val,
                callee_key,
                arg_index,
                is_value,
                materialize,
            );
            let bb_else_end = b.current_block();

            let then_is_nil = matches!(then_val.node, Expr::NilArgument);
            let else_is_nil = matches!(else_val.node, Expr::NilArgument);
            let t_ty = b
                .func()
                .value_type(t_arg.value)
                .unwrap_or(IrType::Int(IntWidth::I64));
            let e_ty = b
                .func()
                .value_type(e_arg.value)
                .unwrap_or(IrType::Int(IntWidth::I64));
            let slot_ty = if then_is_nil && !else_is_nil {
                e_ty
            } else if else_is_nil && !then_is_nil {
                t_ty
            } else if then_is_nil && else_is_nil && !is_value {
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))
            } else {
                t_ty
            };
            let merged_value = b.add_block_param(bb_merge, slot_ty.clone());
            let merged_presence = b.add_block_param(bb_merge, IrType::Bool);
            let merged_character_len =
                if t_arg.character_len.is_some() || e_arg.character_len.is_some() {
                    Some(b.add_block_param(bb_merge, IrType::Int(IntWidth::I64)))
                } else {
                    None
                };
            let owned_count = t_arg
                .owned_character_bases
                .len()
                .max(e_arg.owned_character_bases.len());
            let mut merged_owned_bases = Vec::with_capacity(owned_count);
            for _ in 0..owned_count {
                merged_owned_bases.push(
                    b.add_block_param(bb_merge, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))),
                );
            }

            b.set_block(bb_then_end);
            let t_val = conform_condarg_to_slot(b, t_arg.value, &slot_ty);
            let mut then_args = vec![t_val, t_arg.present];
            if merged_character_len.is_some() {
                then_args.push(t_arg.character_len.unwrap_or_else(|| b.const_i64(0)));
            }
            for index in 0..owned_count {
                then_args.push(
                    t_arg
                        .owned_character_bases
                        .get(index)
                        .copied()
                        .unwrap_or_else(|| null_character_owner(b)),
                );
            }
            b.branch(bb_merge, then_args);

            b.set_block(bb_else_end);
            let e_val = conform_condarg_to_slot(b, e_arg.value, &slot_ty);
            let mut else_args = vec![e_val, e_arg.present];
            if merged_character_len.is_some() {
                else_args.push(e_arg.character_len.unwrap_or_else(|| b.const_i64(0)));
            }
            for index in 0..owned_count {
                else_args.push(
                    e_arg
                        .owned_character_bases
                        .get(index)
                        .copied()
                        .unwrap_or_else(|| null_character_owner(b)),
                );
            }
            b.branch(bb_merge, else_args);

            b.set_block(bb_merge);
            LoweredCallArg {
                value: merged_value,
                present: merged_presence,
                character_len: merged_character_len,
                owned_character_bases: merged_owned_bases,
            }
        }
        _ => {
            let present = call_actual_presence(b, locals, e);
            let must_guard_payload = is_value
                && matches!(
                    &e.node,
                    Expr::Name { name }
                        if locals.contains_key(&optional_value_presence_local_name(name))
                );
            if !must_guard_payload {
                return LoweredCallArg::materialized(materialize(b, e), present);
            }

            let bb_present = b.create_block("optional_value_present");
            let bb_absent = b.create_block("optional_value_absent");
            let bb_merge = b.create_block("optional_value_merge");
            b.cond_branch(present, bb_present, vec![], bb_absent, vec![]);

            b.set_block(bb_present);
            let present_arg = materialize(b, e);
            let present_end = b.current_block();
            let slot_ty = b
                .func()
                .value_type(present_arg.value)
                .unwrap_or(IrType::Int(IntWidth::I64));
            let merged_value = b.add_block_param(bb_merge, slot_ty.clone());
            let merged_character_len = present_arg
                .character_len
                .map(|_| b.add_block_param(bb_merge, IrType::Int(IntWidth::I64)));
            let mut merged_owned_bases =
                Vec::with_capacity(present_arg.owned_character_bases.len());
            for _ in &present_arg.owned_character_bases {
                merged_owned_bases.push(
                    b.add_block_param(bb_merge, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))),
                );
            }

            b.set_block(bb_absent);
            let absent_value = missing_optional_call_arg(b, st, callee_key, arg_index, is_value);
            let absent_value = conform_condarg_to_slot(b, absent_value, &slot_ty);
            let mut absent_args = vec![absent_value];
            if merged_character_len.is_some() {
                absent_args.push(b.const_i64(0));
            }
            for _ in &merged_owned_bases {
                absent_args.push(null_character_owner(b));
            }
            b.branch(bb_merge, absent_args);

            b.set_block(present_end);
            let mut present_args = vec![present_arg.value];
            if merged_character_len.is_some() {
                present_args.push(present_arg.character_len.unwrap_or_else(|| b.const_i64(0)));
            }
            present_args.extend(present_arg.owned_character_bases);
            b.branch(bb_merge, present_args);

            b.set_block(bb_merge);
            LoweredCallArg {
                value: merged_value,
                present,
                character_len: merged_character_len,
                owned_character_bases: merged_owned_bases,
            }
        }
    }
}

// (print_item_contains_proc_call was removed with the list-directed
// fallback: the format runtime's FMT_CTX is a stack, so nested I/O in
// output-item evaluation no longer clobbers the outer format state.)

/// Lower `target = (cond ? then_val : else_val)` for an array-valued
/// conditional by branching on the condition and reusing the ordinary
/// assignment lowering on each arm. F2023 short-circuit holds: exactly one
/// arm's assignment is reached. A chained conditional (`c1 ? a : c2 ? b : c`)
/// recurses — the else-arm is itself a conditional assignment.
fn lower_array_conditional_assign(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    target: &crate::ast::expr::SpannedExpr,
    cond: &crate::ast::expr::SpannedExpr,
    then_val: &crate::ast::expr::SpannedExpr,
    else_val: &crate::ast::expr::SpannedExpr,
) {
    let assign_arm = |arm: &crate::ast::expr::SpannedExpr| -> SpannedStmt {
        crate::ast::Spanned::new(
            Stmt::Assignment {
                target: target.clone(),
                value: arm.clone(),
            },
            target.span,
        )
    };
    // Constant condition folds to the chosen arm with no extra blocks.
    if let Expr::LogicalLiteral { value, .. } = &cond.node {
        let arm = if *value { then_val } else { else_val };
        lower_stmt(b, ctx, &assign_arm(arm));
        return;
    }
    let cond_raw = super::expr::lower_expr_ctx(b, ctx, cond);
    let cond_val = coerce_to_type(b, cond_raw, &IrType::Bool);
    let bb_then = b.create_block("arrcond_then");
    let bb_else = b.create_block("arrcond_else");
    let bb_done = b.create_block("arrcond_done");
    b.cond_branch(cond_val, bb_then, vec![], bb_else, vec![]);

    b.set_block(bb_then);
    lower_stmt(b, ctx, &assign_arm(then_val));
    b.branch(bb_done, vec![]);

    b.set_block(bb_else);
    lower_stmt(b, ctx, &assign_arm(else_val));
    b.branch(bb_done, vec![]);

    b.set_block(bb_done);
}

/// Lower a single statement.
pub(crate) fn lower_stmt(b: &mut FuncBuilder, ctx: &mut LowerCtx, stmt: &SpannedStmt) {
    match &stmt.node {
        Stmt::Assignment { target, value } => {
            // F77 statement-function definitions look like
            // `name(p1, p2, ...) = expr` — sema records the body in a
            // side table and flips the symbol kind to Function. There
            // is no IR to emit for the definition itself; call sites
            // inline-substitute the body in `lower_expr_full`.
            if let Expr::FunctionCall { callee, .. } = &target.node {
                if let Expr::Name { name } = &callee.node {
                    if ctx.lookup_statement_function(name).is_some() {
                        return;
                    }
                }
            }
            // F2023 array-valued conditional expression as an assignment
            // RHS (`x = (c ? a : b)` with array arms). Lower it as a
            // runtime branch that performs the per-arm assignment through
            // the normal (shape-correct) assignment path, instead of
            // building a descriptor merge. Each arm reuses every existing
            // array path — allocatable auto-realloc, sections, constructors
            // — so no array shape is mishandled. Scalar conditionals fall
            // through to the scalar expression path below.
            if let Expr::ConditionalExpr {
                cond,
                then_val,
                else_val,
            } = &value.node
            {
                if actual_expr_rank(value, &ctx.locals, ctx.st, Some(ctx.type_layouts))
                    .is_some_and(|r| r > 0)
                {
                    lower_array_conditional_assign(b, ctx, target, cond, then_val, else_val);
                    return;
                }
            }
            match &target.node {
                Expr::Name { name } => {
                    let key = name.to_lowercase();
                    // Defined assignment: INTERFACE ASSIGNMENT(=) covers
                    // cases where the LHS and RHS types differ or the
                    // user defined a custom store semantics. When we
                    // resolve a specific, lower the call and return —
                    // the default type-matched paths below would either
                    // memcpy garbage or fall through silently.
                    if try_defined_assignment(b, ctx, &key, value) {
                        return;
                    }
                    if let Some(info) = ctx.locals.get(&key).cloned() {
                        // Route fixed-size (non-descriptor) array assignments
                        // to lower_array_assign when the RHS is an array
                        // expression — array+scalar broadcasts and array
                        // constructors need element-wise lowering, not the
                        // scalar store fallback below. Skip derived/character
                        // arrays so their specialized paths run instead.
                        if local_is_array_like(&info)
                            && !local_uses_array_descriptor(&info)
                            && info.derived_type.is_none()
                            && info.char_kind == CharKind::None
                            && (matches!(value.node, Expr::ArrayConstructor { .. })
                                || expr_contains_array_constructor(value)
                                || expr_is_transfer_array_call(value))
                        {
                            lower_array_assign(b, ctx, name, &info, value);
                            return;
                        }
                        // `arr = transfer(src, mold, N)` for an
                        // allocatable rank-1 array. The source bits get
                        // memcpy'd into a freshly-allocated descriptor
                        // (handled by `try_lower_transfer_into_array`
                        // inside lower_array_assign).  Without this
                        // route, the generic function-result path treated
                        // transfer's bit-cast bytes as a source
                        // descriptor pointer and segfaulted in
                        // `afs_assign_allocatable` on the first character
                        // byte (e.g. 0x6d for 'm' in stdlib_hashmaps).
                        // SIZE may be runtime-evaluated; the descriptor
                        // path inside try_lower_transfer_into_array
                        // lowers it via `lower_expr_ctx_tl`.
                        if local_is_array_like(&info)
                            && local_uses_array_descriptor(&info)
                            && info.derived_type.is_none()
                            && info.char_kind == CharKind::None
                            && expr_is_transfer_array_call_dynamic(value)
                        {
                            lower_array_assign(b, ctx, name, &info, value);
                            return;
                        }
                        if local_is_array_like(&info)
                            && (info.char_kind != CharKind::None
                                || descriptor_backed_runtime_char_array(&info))
                        {
                            lower_array_assign(b, ctx, name, &info, value);
                            return;
                        }
                        match &info.char_kind {
                            CharKind::Fixed(len) => {
                                // Fixed-length character assignment: copy with space padding.
                                // Get source pointer and length from the expression.
                                let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                if let Some((dest_ptr, dest_len)) = local_char_ptr_and_len(b, &info)
                                {
                                    b.call(
                                        FuncRef::External("afs_assign_char_fixed".into()),
                                        vec![dest_ptr, dest_len, src_ptr, src_len],
                                        IrType::Void,
                                    );
                                } else {
                                    let dest_len = b.const_i64(*len);
                                    b.call(
                                        FuncRef::External("afs_assign_char_fixed".into()),
                                        vec![info.addr, dest_len, src_ptr, src_len],
                                        IrType::Void,
                                    );
                                }
                                deallocate_owned_string_expr_temp(
                                    b,
                                    &ctx.locals,
                                    value,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                    src_ptr,
                                );
                            }
                            CharKind::FixedRuntime { len_addr } => {
                                let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                let (dest_ptr, dest_len) =
                                    fixed_runtime_char_ptr_and_len(b, &info, *len_addr);
                                b.call(
                                    FuncRef::External("afs_assign_char_fixed".into()),
                                    vec![dest_ptr, dest_len, src_ptr, src_len],
                                    IrType::Void,
                                );
                                deallocate_owned_string_expr_temp(
                                    b,
                                    &ctx.locals,
                                    value,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                    src_ptr,
                                );
                            }
                            CharKind::Deferred => {
                                let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                let desc = string_descriptor_addr(b, &info);
                                if info.is_pointer {
                                    let (dest_ptr, dest_len) =
                                        load_string_descriptor_substring_view(b, desc);
                                    b.call(
                                        FuncRef::External("afs_assign_char_fixed".into()),
                                        vec![dest_ptr, dest_len, src_ptr, src_len],
                                        IrType::Void,
                                    );
                                } else {
                                    // Deferred-length allocatables keep reallocation semantics.
                                    b.call(
                                        FuncRef::External("afs_assign_char_deferred".into()),
                                        vec![desc, src_ptr, src_len],
                                        IrType::Void,
                                    );
                                }
                                deallocate_owned_string_expr_temp(
                                    b,
                                    &ctx.locals,
                                    value,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                    src_ptr,
                                );
                            }
                            CharKind::AssumedLen { len_addr } => {
                                // Assumed-length dummy assignment: use
                                // the hidden-length param as the
                                // destination length.
                                let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                let outer = b.load(info.addr);
                                let dest_ptr = b.load_typed(
                                    outer,
                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                );
                                let dest_len = b.load(*len_addr);
                                b.call(
                                    FuncRef::External("afs_assign_char_fixed".into()),
                                    vec![dest_ptr, dest_len, src_ptr, src_len],
                                    IrType::Void,
                                );
                                deallocate_owned_string_expr_temp(
                                    b,
                                    &ctx.locals,
                                    value,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                    src_ptr,
                                );
                            }
                            CharKind::None => {
                                if local_fixed_char_allocatable_scalar_len(&info).is_some() {
                                    let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                    if let Some((dest_ptr, dest_len)) =
                                        char_addr_and_runtime_len(b, target, &ctx.locals)
                                    {
                                        b.call(
                                            FuncRef::External("afs_assign_char_fixed".into()),
                                            vec![dest_ptr, dest_len, src_ptr, src_len],
                                            IrType::Void,
                                        );
                                    }
                                    deallocate_owned_string_expr_temp(
                                        b,
                                        &ctx.locals,
                                        value,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                        src_ptr,
                                    );
                                } else if local_uses_array_descriptor(&info)
                                    && local_declared_rank(&info) == 0
                                    && info.derived_type.is_some()
                                {
                                    let desc = array_descriptor_addr(b, &info);
                                    let mut rhs_scalar_desc = if info.is_class {
                                        match &value.node {
                                            Expr::Name { name } => ctx
                                                .locals
                                                .get(&name.to_lowercase())
                                                .filter(|src_info| {
                                                    src_info.is_class
                                                        && local_uses_array_descriptor(src_info)
                                                        && local_declared_rank(src_info) == 0
                                                })
                                                .map(|src_info| array_descriptor_addr(b, src_info)),
                                            _ => None,
                                        }
                                    } else {
                                        None
                                    };
                                    let dynamic_source_copy_plan = rhs_scalar_desc.and_then(|_| {
                                        info.derived_type.as_ref().map(|base| {
                                            ScalarAllocSourceCopyPlan::Dynamic(base.clone())
                                        })
                                    });
                                    let mut rhs_scalar_snapshot = None;
                                    if let (
                                        Some(source_desc),
                                        Some(ScalarAllocSourceCopyPlan::Dynamic(base_type)),
                                    ) = (rhs_scalar_desc, dynamic_source_copy_plan.as_ref())
                                    {
                                        let (stable_source, snapshot_desc) =
                                            stabilize_dynamic_scalar_assignment_source(
                                                b,
                                                desc,
                                                source_desc,
                                                base_type,
                                                ctx.type_layouts,
                                            );
                                        rhs_scalar_desc = Some(stable_source);
                                        rhs_scalar_snapshot = Some(snapshot_desc);
                                    }
                                    let assign_type_name =
                                        if info.is_class && rhs_scalar_desc.is_none() {
                                            expr_type_layout(
                                                value,
                                                Some(&ctx.locals),
                                                ctx.st,
                                                ctx.type_layouts,
                                            )
                                            .filter(|layout| !layout.is_abstract)
                                            .map(|layout| layout.name.clone())
                                        } else {
                                            None
                                        }
                                        .or_else(|| info.derived_type.clone());
                                    let allocated = b.call(
                                        FuncRef::External("afs_allocated".into()),
                                        vec![desc],
                                        IrType::Int(IntWidth::I32),
                                    );
                                    let zero32 = b.const_i32(0);
                                    let assign_stat = b.alloca(IrType::Int(IntWidth::I32));
                                    b.store(zero32, assign_stat);
                                    let needs_alloc = b.icmp(CmpOp::Eq, allocated, zero32);
                                    let needs_storage_alloc = if let Some(source_desc) =
                                        rhs_scalar_desc
                                    {
                                        let current_elem_size =
                                            load_array_desc_i64_field(b, desc, 8);
                                        let source_elem_size =
                                            load_array_desc_i64_field(b, source_desc, 8);
                                        let size_mismatch =
                                            b.icmp(CmpOp::Ne, current_elem_size, source_elem_size);
                                        let current_tag = load_array_desc_type_tag(b, desc);
                                        let source_tag = load_array_desc_type_tag(b, source_desc);
                                        let tag_mismatch =
                                            b.icmp(CmpOp::Ne, current_tag, source_tag);
                                        let storage_mismatch = b.or(size_mismatch, tag_mismatch);
                                        b.or(needs_alloc, storage_mismatch)
                                    } else if info.is_class {
                                        if let Some(ref tn) = assign_type_name {
                                            if let Some(layout) = ctx.type_layouts.get(tn) {
                                                let current_elem_size =
                                                    load_array_desc_i64_field(b, desc, 8);
                                                let target_elem_size =
                                                    b.const_i64(layout.size as i64);
                                                let size_mismatch = b.icmp(
                                                    CmpOp::Ne,
                                                    current_elem_size,
                                                    target_elem_size,
                                                );
                                                let current_tag = load_array_desc_type_tag(b, desc);
                                                let target_tag =
                                                    b.const_i64(layout.type_tag as i64);
                                                let tag_mismatch =
                                                    b.icmp(CmpOp::Ne, current_tag, target_tag);
                                                let storage_mismatch =
                                                    b.or(size_mismatch, tag_mismatch);
                                                b.or(needs_alloc, storage_mismatch)
                                            } else {
                                                needs_alloc
                                            }
                                        } else {
                                            needs_alloc
                                        }
                                    } else {
                                        needs_alloc
                                    };
                                    let rhs_result_temp_type =
                                        finalizable_function_result_type_name(ctx, value);
                                    let rhs_scalar_value = if rhs_scalar_desc.is_none() {
                                        let raw = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                        Some(if let Some(ref tn) = assign_type_name {
                                            stabilize_finalized_assignment_rhs(b, ctx, tn, raw)
                                        } else {
                                            raw
                                        })
                                    } else {
                                        None
                                    };
                                    let alloc_bb = b.create_block("scalar_derived_assign_alloc");
                                    let finalize_existing_bb =
                                        b.create_block("scalar_derived_assign_finalize");
                                    let copy_bb = b.create_block("scalar_derived_assign_copy");
                                    let done_bb = b.create_block("scalar_derived_assign_done");
                                    b.cond_branch(
                                        needs_storage_alloc,
                                        alloc_bb,
                                        vec![],
                                        finalize_existing_bb,
                                        vec![],
                                    );

                                    b.set_block(alloc_bb);
                                    let already_allocated = b.icmp(CmpOp::Ne, allocated, zero32);
                                    let dealloc_bb =
                                        b.create_block("scalar_derived_assign_realloc_dealloc");
                                    let do_alloc_bb =
                                        b.create_block("scalar_derived_assign_do_alloc");
                                    b.cond_branch(
                                        already_allocated,
                                        dealloc_bb,
                                        vec![],
                                        do_alloc_bb,
                                        vec![],
                                    );

                                    b.set_block(dealloc_bb);
                                    if let Some(type_name) = info.derived_type.as_ref() {
                                        if let Some(layout) = ctx.type_layouts.get(type_name) {
                                            prepare_descriptor_assignment_lhs(
                                                b,
                                                ctx,
                                                desc,
                                                info.is_class,
                                                layout,
                                                assign_stat,
                                            );
                                        }
                                    }
                                    b.store(zero32, assign_stat);
                                    b.call(
                                        FuncRef::External("afs_deallocate_array".into()),
                                        vec![desc, assign_stat],
                                        IrType::Void,
                                    );
                                    b.branch(do_alloc_bb, vec![]);

                                    b.set_block(do_alloc_bb);
                                    if let Some(source_desc) = rhs_scalar_desc {
                                        b.store(zero32, assign_stat);
                                        b.call(
                                            FuncRef::External("afs_allocate_like".into()),
                                            vec![desc, source_desc, assign_stat],
                                            IrType::Void,
                                        );
                                    } else if let Some(ref tn) = assign_type_name {
                                        if let Some(layout) = ctx.type_layouts.get(tn) {
                                            let elem_size = b.const_i64(layout.size as i64);
                                            let rank_val = b.const_i32(0);
                                            let null_ptr = b.const_i64(0);
                                            b.call(
                                                FuncRef::External("afs_allocate_array".into()),
                                                vec![desc, elem_size, rank_val, null_ptr, null_ptr],
                                                IrType::Void,
                                            );
                                            if derived_layout_needs_runtime_initialization(
                                                layout,
                                                ctx.type_layouts,
                                            ) {
                                                let base_ptr = b.load_typed(
                                                    desc,
                                                    IrType::Ptr(Box::new(IrType::Int(
                                                        IntWidth::I8,
                                                    ))),
                                                );
                                                initialize_derived_storage(
                                                    b,
                                                    base_ptr,
                                                    layout,
                                                    ctx.type_layouts,
                                                );
                                            }
                                        }
                                    }
                                    b.branch(copy_bb, vec![]);

                                    b.set_block(finalize_existing_bb);
                                    if let Some(type_name) = info.derived_type.as_ref() {
                                        if let Some(layout) = ctx.type_layouts.get(type_name) {
                                            prepare_descriptor_assignment_lhs(
                                                b,
                                                ctx,
                                                desc,
                                                info.is_class,
                                                layout,
                                                assign_stat,
                                            );
                                        }
                                    }
                                    b.branch(copy_bb, vec![]);

                                    b.set_block(copy_bb);
                                    if let Some(source_desc) = rhs_scalar_desc {
                                        emit_allocatable_source_copy_on_success(
                                            b,
                                            assign_stat,
                                            assign_stat,
                                            desc,
                                            source_desc,
                                            false,
                                            None,
                                            dynamic_source_copy_plan.as_ref(),
                                            ctx.type_layouts,
                                            None,
                                        );
                                        if let Some(snapshot_desc) = rhs_scalar_snapshot {
                                            if let Some(layout) = info
                                                .derived_type
                                                .as_deref()
                                                .and_then(|name| ctx.type_layouts.get(name))
                                                .cloned()
                                            {
                                                discard_dynamic_assignment_snapshot(
                                                    b,
                                                    snapshot_desc,
                                                    &layout,
                                                    ctx.type_layouts,
                                                );
                                            }
                                        }
                                    } else {
                                        let val = rhs_scalar_value.expect(
                                            "scalar derived assignment value lowered before branch",
                                        );
                                        let dest = derived_storage_addr(b, &info);
                                        if let Some(ref tn) = assign_type_name {
                                            emit_derived_value_copy(
                                                b,
                                                ctx.type_layouts,
                                                tn,
                                                dest,
                                                val,
                                            );
                                        }
                                        if let Some(ref temp_type) = rhs_result_temp_type {
                                            finalize_assignment_lhs(b, ctx, temp_type, val);
                                        }
                                        // Scalar descriptor-backed TYPE allocatables keep their
                                        // dynamic type identity in the descriptor metadata, so
                                        // constructor/function-result assignment must restamp the
                                        // concrete metadata after copying the value bytes.
                                        if let Some(tag) = derived_type_tag_value(
                                            b,
                                            assign_type_name.as_deref(),
                                            ctx.type_layouts,
                                        ) {
                                            store_array_desc_type_tag(b, desc, tag);
                                        }
                                        if let Some(lookup) = derived_type_vtable_value(
                                            b,
                                            assign_type_name.as_deref(),
                                            ctx.type_layouts,
                                        ) {
                                            store_array_desc_vtable_ptr(b, desc, lookup);
                                        }
                                    }
                                    b.branch(done_bb, vec![]);
                                    b.set_block(done_bb);
                                } else if is_unlimited_polymorphic_local(&info)
                                    && local_declared_rank(&info) == 0
                                    && ctx
                                        .st
                                        .find_symbol_any_scope(&key)
                                        .map(|s| s.attrs.allocatable)
                                        .unwrap_or(info.allocatable)
                                {
                                    let dst = array_descriptor_addr(b, &info);
                                    let src = class_star_descriptor_source(b, ctx, value);
                                    let owned_bases = b.take_owned_string_temp_bases(src);
                                    copy_unlimited_polymorphic_allocatable_descriptor(
                                        b,
                                        dst,
                                        src,
                                        ctx.type_layouts,
                                    );
                                    deallocate_owned_string_bases(b, &owned_bases);
                                } else if !info.dims.is_empty() || info.allocatable {
                                    if try_lower_elemental_array_assign(b, ctx, name, &info, value)
                                    {
                                        return;
                                    }
                                    if try_lower_defined_operator_array_assign(
                                        b, ctx, name, &info, value,
                                    ) {
                                        return;
                                    }
                                    let defined_unary_array_result =
                                        synth_defined_unary_array_result_call(ctx, value);
                                    let array_rhs =
                                        defined_unary_array_result.as_ref().unwrap_or(value);
                                    if let Expr::FunctionCall {
                                        callee,
                                        args: call_args,
                                    } = &array_rhs.node
                                    {
                                        // F2018 §9.5.3.3 vector subscript: when the
                                        // callee resolves to a local array (not a
                                        // function), `x(col)` is gather, not a call.
                                        // Route through lower_array_assign so the
                                        // scalarization path picks it up.
                                        let callee_is_local_array =
                                            if let Expr::Name { name: cname } = &callee.node {
                                                ctx.locals
                                                    .get(&cname.to_lowercase())
                                                    .is_some_and(local_is_array_like)
                                            } else {
                                                false
                                            };
                                        // F2018 §16.9: elemental intrinsic applied
                                        // to an array actual returns an array. The
                                        // direct `lower_expr_ctx_tl` path treats
                                        // the call as scalar and emits e.g.
                                        // `b.fsqrt(array_descriptor)` — wrong.
                                        // Route to lower_array_assign so the
                                        // scalarization path expands the elemental
                                        // call element-wise.
                                        let callee_is_elemental_array_intrinsic =
                                            if let Expr::Name { name: cname } = &callee.node {
                                                let lname = cname.to_lowercase();
                                                let whole_array_scalar_intrinsic =
                                                    crate::sema::validate::is_intrinsic_name(
                                                        &lname,
                                                    ) && is_array_reducing_intrinsic(&lname)
                                                        && !user_callable_shadows_intrinsic(
                                                            ctx.st,
                                                            ctx.proc_scope_id,
                                                            b.func().name.as_str(),
                                                            &lname,
                                                        );
                                                let direct_elemental = !whole_array_scalar_intrinsic
                                                    && (is_elemental_math_intrinsic(cname)
                                                        || ctx.elemental_funcs.contains(&lname)
                                                        || ctx
                                                            .st
                                                            .find_symbol_any_scope(&lname)
                                                            .is_some_and(|s| s.attrs.elemental));
                                                let generic_specifics_elemental = !direct_elemental
                                                    && resolved_named_callee_is_elemental(
                                                        b,
                                                        &ctx.locals,
                                                        cname,
                                                        call_args,
                                                        ctx.st,
                                                        Some(ctx.type_layouts),
                                                        Some(ctx.internal_funcs),
                                                        Some(ctx.contained_host_refs),
                                                        Some(ctx.descriptor_params),
                                                    );
                                                let is_elemental =
                                                    direct_elemental || generic_specifics_elemental;
                                                is_elemental
                                                    && call_args.iter().any(|arg| {
                                                        matches!(
                                                            &arg.value,
                                                            crate::ast::expr::SectionSubscript::Element(e)
                                                                if actual_expr_rank(
                                                                    e,
                                                                    &ctx.locals,
                                                                    ctx.st,
                                                                    Some(ctx.type_layouts),
                                                                )
                                                                .is_some_and(|rank| rank > 0)
                                                                    || expr_contains_array_refs(e, &ctx.locals)
                                                                    || expr_contains_array_constructor(e)
                                                        )
                                                    })
                                            } else {
                                                false
                                            };
                                        // F2018 §16.9: transformational intrinsics that
                                        // synthesize a fresh array result (RESHAPE, MATMUL,
                                        // TRANSPOSE, SHAPE).  Routing through
                                        // lower_array_assign lets lower_array_expr_descriptor's
                                        // dedicated arms allocate and fill the descriptor
                                        // instead of the generic call path emitting
                                        // unresolved `_reshape`/`_transpose` externals.
                                        let callee_is_transformational_intrinsic =
                                            if let Expr::Name { name: cname } = &callee.node {
                                                let lname = cname.to_ascii_lowercase();
                                                matches!(
                                                    lname.as_str(),
                                                    "reshape"
                                                        | "matmul"
                                                        | "transpose"
                                                        | "shape"
                                                        | "pack"
                                                        | "spread"
                                                        | "cshift"
                                                        | "eoshift"
                                                        | "transfer"
                                                        // cmplx(re, im, kind) over real
                                                        // arrays: lower_array_expr_descriptor
                                                        // has a dedicated arm (afs_array_cmplx).
                                                        // Without this entry the assignment
                                                        // routes through lower_expr_ctx_tl,
                                                        // which calls scalar lower_intrinsic
                                                        // with null-pointer probes and emits
                                                        // a single complex(4) const-zero
                                                        // buffer — wrong shape and wrong kind.
                                                        | "cmplx"
                                                        // merge(t, f, mask) over arrays:
                                                        // lower_array_merge_descriptor
                                                        // materializes a temp via per-element
                                                        // select. Without this entry the
                                                        // FunctionCall arm picks up scalar
                                                        // intrinsic merge, which emits
                                                        // `select` on pointer operands and
                                                        // hands a scalar f64 to the assignment
                                                        // memcpy as a "source descriptor" —
                                                        // SEGV on dereference. Surfaced in
                                                        // stdlib's iterative solvers
                                                        // (solve_cg/bicgstab/pcg).
                                                        | "merge"
                                                ) || (
                                                    // sum(arr, dim) is rank-N-1: route to
                                                    // lower_array_assign so the sum-dim arm
                                                    // in lower_array_expr_descriptor fills
                                                    // the result descriptor. Plain sum(arr)
                                                    // is scalar; that arm returns None and
                                                    // assignment falls through to scalar
                                                    // broadcast.
                                                    lname == "sum"
                                                        && call_args.iter().enumerate().any(
                                                            |(i, a)| {
                                                                let kw = a
                                                                    .keyword
                                                                    .as_deref()
                                                                    .map(|s| s.to_lowercase());
                                                                matches!(kw.as_deref(), Some("dim"))
                                                                    || (i == 1 && kw.is_none())
                                                            },
                                                        )
                                                ) || (
                                                    // count(mask, dim) is rank-N-1 integer
                                                    // array: same routing as sum(arr, dim).
                                                    // Without this, the scalar logical-
                                                    // reduction path returns a single i32
                                                    // total and the array-assign treats it
                                                    // as a source descriptor, dereferencing
                                                    // a tiny address (e.g. 0x3) and aborting
                                                    // in afs_assign_allocatable. Surfaced
                                                    // in stdlib_stats var_mask_2_*.
                                                    lname == "count"
                                                        && call_args.iter().enumerate().any(
                                                            |(i, a)| {
                                                                let kw = a
                                                                    .keyword
                                                                    .as_deref()
                                                                    .map(|s| s.to_lowercase());
                                                                matches!(kw.as_deref(), Some("dim"))
                                                                    || (i == 1 && kw.is_none())
                                                            },
                                                        )
                                                ) || (
                                                    // maxval/minval(arr, dim) are also rank-N-1
                                                    // when the source rank is greater than one.
                                                    // Keep rank-1 reductions on the scalar path
                                                    // so nested forms such as
                                                    // maxval(sum(abs(A), dim=1), 1) still return
                                                    // a scalar.
                                                    matches!(lname.as_str(), "maxval" | "minval")
                                                        && actual_expr_rank(
                                                            array_rhs,
                                                            &ctx.locals,
                                                            ctx.st,
                                                            Some(ctx.type_layouts),
                                                        )
                                                        .is_some_and(|rank| rank > 0)
                                                )
                                            } else {
                                                false
                                            };
                                        // Scalar-returning intrinsic broadcast to a whole array:
                                        // `x = ieee_value(1.0, NaN)`, `x = epsilon(1.0)`,
                                        // `x = huge(1.0)`. The fall-through path lowers the
                                        // call as a scalar then treats the result as a source
                                        // descriptor pointer — IR verifier catches "load from
                                        // non-pointer fN" on fixed-size dests, and SEGV inside
                                        // afs_assign_allocatable on descriptor-backed dests
                                        // (stdlib's pinv_s_operator on the linalg-error path
                                        // `pinva = ieee_value(1.0_sp, ieee_quiet_nan)`).
                                        // Restricted to a known set of always-scalar
                                        // intrinsics — extending it broadly mis-routes user
                                        // functions that legitimately return arrays.
                                        let callee_is_scalar_broadcast_intrinsic =
                                            local_is_array_like(&info)
                                                && !callee_is_local_array
                                                && !callee_is_elemental_array_intrinsic
                                                && !callee_is_transformational_intrinsic
                                                && {
                                                    if let Expr::Name { name: cname } = &callee.node
                                                    {
                                                        let lk = cname.to_lowercase();
                                                        matches!(
                                                            lk.as_str(),
                                                            "ieee_value"
                                                                | "epsilon"
                                                                | "huge"
                                                                | "tiny"
                                                                | "radix"
                                                                | "digits"
                                                                | "precision"
                                                                | "range"
                                                                | "minexponent"
                                                                | "maxexponent"
                                                        )
                                                            || (lk == "len"
                                                                && crate::sema::validate::is_intrinsic_name(
                                                                    &lk,
                                                                )
                                                                && !user_callable_shadows_intrinsic(
                                                                    ctx.st,
                                                                    ctx.proc_scope_id,
                                                                    b.func().name.as_str(),
                                                                    &lk,
                                                                ))
                                                    } else {
                                                        false
                                                    }
                                                };
                                        if callee_is_local_array
                                            || callee_is_elemental_array_intrinsic
                                            || callee_is_transformational_intrinsic
                                            || callee_is_scalar_broadcast_intrinsic
                                        {
                                            lower_array_assign(b, ctx, name, &info, array_rhs);
                                            return;
                                        }
                                        if let Expr::Name { name: callee_name } = &callee.node {
                                            let callee_key = callee_name.to_lowercase();
                                            if ctx.alloc_return_funcs.contains(&callee_key) {
                                                // sret call. When dest is descriptor-backed we
                                                // can let the callee write straight in. When
                                                // dest is a fixed-shape stack buffer (e.g.
                                                // `real :: r(10)`) `array_descriptor_addr`
                                                // returns the buffer itself, but the callee
                                                // expects a 392-byte descriptor — handing it
                                                // the buffer corrupts the caller frame the
                                                // moment the callee touches dims/flags. Allocate
                                                // a real descriptor temp, call into it, copy
                                                // the bytes back, and deallocate the heap
                                                // result.
                                                if local_uses_array_descriptor(&info) {
                                                    lower_array_assign(
                                                        b, ctx, name, &info, array_rhs,
                                                    );
                                                } else {
                                                    let src_elem_ty =
                                                        array_function_result_elem_type(
                                                            b,
                                                            &ctx.locals,
                                                            callee,
                                                            call_args,
                                                            ctx.st,
                                                            Some(ctx.type_layouts),
                                                            Some(ctx.internal_funcs),
                                                            Some(ctx.contained_host_refs),
                                                            Some(ctx.descriptor_params),
                                                        );
                                                    let tmp_desc = b.alloca(IrType::Array(
                                                        Box::new(IrType::Int(IntWidth::I8)),
                                                        392,
                                                    ));
                                                    let zero32 = b.const_i32(0);
                                                    let descriptor_bytes = b.const_i64(392);
                                                    b.call(
                                                        FuncRef::External("memset".into()),
                                                        vec![tmp_desc, zero32, descriptor_bytes],
                                                        IrType::Ptr(Box::new(IrType::Int(
                                                            IntWidth::I8,
                                                        ))),
                                                    );
                                                    lower_alloc_return_call_into_desc(
                                                        b,
                                                        ctx,
                                                        tmp_desc,
                                                        callee_name,
                                                        call_args,
                                                    );
                                                    copy_array_result_to_fixed_dest(
                                                        b,
                                                        &info,
                                                        tmp_desc,
                                                        src_elem_ty.as_ref(),
                                                    );
                                                    let stat = b.alloca(IrType::Int(IntWidth::I32));
                                                    b.store(zero32, stat);
                                                    b.call(
                                                        FuncRef::External(
                                                            "afs_deallocate_array".into(),
                                                        ),
                                                        vec![tmp_desc, stat],
                                                        IrType::Void,
                                                    );
                                                }
                                            } else {
                                                if actual_expr_rank(
                                                    array_rhs,
                                                    &ctx.locals,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                ) == Some(0)
                                                {
                                                    lower_array_assign(
                                                        b, ctx, name, &info, array_rhs,
                                                    );
                                                    return;
                                                }
                                                // Function returns a temp descriptor. Mirror
                                                // the alloc_return path: when dest is a real
                                                // descriptor, route through afs_assign_allocatable;
                                                // when dest is a fixed-shape buffer, memcpy the
                                                // result bytes in.
                                                let src_elem_ty = array_function_result_elem_type(
                                                    b,
                                                    &ctx.locals,
                                                    callee,
                                                    call_args,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                    Some(ctx.internal_funcs),
                                                    Some(ctx.contained_host_refs),
                                                    Some(ctx.descriptor_params),
                                                );
                                                let src_desc = super::expr::lower_expr_ctx_tl(
                                                    b, ctx, array_rhs,
                                                );
                                                if local_uses_array_descriptor(&info) {
                                                    let dest_desc = array_descriptor_addr(b, &info);
                                                    if descriptor_backed_char_array(&info) {
                                                        let dest_elem_len =
                                                            fixed_char_allocatable_array_elem_len(
                                                                b, &info,
                                                            );
                                                        lower_allocatable_char_array_assign_from_desc(
                                                            b,
                                                            dest_desc,
                                                            src_desc,
                                                            dest_elem_len,
                                                        );
                                                    } else if !info.allocatable {
                                                        copy_array_result_to_descriptor_dest(
                                                            b, &info, src_desc,
                                                        );
                                                    } else {
                                                        let src_kind_tag = src_elem_ty
                                                            .as_ref()
                                                            .and_then(numeric_kind_tag_for_ir_type);
                                                        let dest_kind_tag =
                                                            numeric_kind_tag_for_ir_type(&info.ty);
                                                        if let (Some(sk), Some(dk)) =
                                                            (src_kind_tag, dest_kind_tag)
                                                        {
                                                            if sk != dk {
                                                                let dk_v = b.const_i32(dk);
                                                                let sk_v = b.const_i32(sk);
                                                                b.call(
                                                                    FuncRef::External(
                                                                        "afs_assign_allocatable_convert"
                                                                            .into(),
                                                                    ),
                                                                    vec![
                                                                        dest_desc, src_desc, dk_v,
                                                                        sk_v,
                                                                    ],
                                                                    IrType::Void,
                                                                );
                                                            } else {
                                                                b.call(
                                                                    FuncRef::External(
                                                                        "afs_assign_allocatable"
                                                                            .into(),
                                                                    ),
                                                                    vec![dest_desc, src_desc],
                                                                    IrType::Void,
                                                                );
                                                            }
                                                        } else {
                                                            b.call(
                                                                FuncRef::External(
                                                                    "afs_assign_allocatable".into(),
                                                                ),
                                                                vec![dest_desc, src_desc],
                                                                IrType::Void,
                                                            );
                                                        }
                                                    }
                                                } else {
                                                    copy_array_result_to_fixed_dest(
                                                        b,
                                                        &info,
                                                        src_desc,
                                                        src_elem_ty.as_ref(),
                                                    );
                                                }
                                                let stat = b.alloca(IrType::Int(IntWidth::I32));
                                                let zero32 = b.const_i32(0);
                                                b.store(zero32, stat);
                                                b.call(
                                                    FuncRef::External(
                                                        "afs_deallocate_array".into(),
                                                    ),
                                                    vec![src_desc, stat],
                                                    IrType::Void,
                                                );
                                            }
                                        } else {
                                            if actual_expr_rank(
                                                array_rhs,
                                                &ctx.locals,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            ) == Some(0)
                                            {
                                                lower_array_assign(b, ctx, name, &info, array_rhs);
                                                return;
                                            }
                                            // Indirect callee: same dest split as above.
                                            let src_elem_ty = array_function_result_elem_type(
                                                b,
                                                &ctx.locals,
                                                callee,
                                                call_args,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                            );
                                            let src_desc =
                                                super::expr::lower_expr_ctx_tl(b, ctx, array_rhs);
                                            if local_uses_array_descriptor(&info) {
                                                let dest_desc = array_descriptor_addr(b, &info);
                                                if descriptor_backed_char_array(&info) {
                                                    let dest_elem_len =
                                                        fixed_char_allocatable_array_elem_len(
                                                            b, &info,
                                                        );
                                                    lower_allocatable_char_array_assign_from_desc(
                                                        b,
                                                        dest_desc,
                                                        src_desc,
                                                        dest_elem_len,
                                                    );
                                                } else if !info.allocatable {
                                                    copy_array_result_to_descriptor_dest(
                                                        b, &info, src_desc,
                                                    );
                                                } else {
                                                    let src_kind_tag = src_elem_ty
                                                        .as_ref()
                                                        .and_then(numeric_kind_tag_for_ir_type);
                                                    let dest_kind_tag =
                                                        numeric_kind_tag_for_ir_type(&info.ty);
                                                    if let (Some(sk), Some(dk)) =
                                                        (src_kind_tag, dest_kind_tag)
                                                    {
                                                        if sk != dk {
                                                            let dk_v = b.const_i32(dk);
                                                            let sk_v = b.const_i32(sk);
                                                            b.call(
                                                                FuncRef::External(
                                                                    "afs_assign_allocatable_convert"
                                                                        .into(),
                                                                ),
                                                                vec![
                                                                    dest_desc, src_desc, dk_v, sk_v,
                                                                ],
                                                                IrType::Void,
                                                            );
                                                        } else {
                                                            b.call(
                                                                FuncRef::External(
                                                                    "afs_assign_allocatable".into(),
                                                                ),
                                                                vec![dest_desc, src_desc],
                                                                IrType::Void,
                                                            );
                                                        }
                                                    } else {
                                                        b.call(
                                                            FuncRef::External(
                                                                "afs_assign_allocatable".into(),
                                                            ),
                                                            vec![dest_desc, src_desc],
                                                            IrType::Void,
                                                        );
                                                    }
                                                }
                                            } else {
                                                copy_array_result_to_fixed_dest(
                                                    b,
                                                    &info,
                                                    src_desc,
                                                    src_elem_ty.as_ref(),
                                                );
                                            }
                                            let stat = b.alloca(IrType::Int(IntWidth::I32));
                                            let zero32 = b.const_i32(0);
                                            b.store(zero32, stat);
                                            b.call(
                                                FuncRef::External("afs_deallocate_array".into()),
                                                vec![src_desc, stat],
                                                IrType::Void,
                                            );
                                        }
                                    } else {
                                        lower_array_assign(b, ctx, name, &info, value);
                                    }
                                } else if info.derived_type.is_some() {
                                    let result_temp_type =
                                        finalizable_function_result_type_name(ctx, value);
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let dest = derived_storage_addr(b, &info);
                                    if let Some(ref tn) = info.derived_type {
                                        let val =
                                            stabilize_finalized_assignment_rhs(b, ctx, tn, val);
                                        finalize_assignment_lhs(b, ctx, tn, dest);
                                        emit_derived_value_copy(b, ctx.type_layouts, tn, dest, val);
                                        if let Some(ref temp_type) = result_temp_type {
                                            finalize_assignment_lhs(b, ctx, temp_type, val);
                                        }
                                    }
                                } else if info.is_pointer {
                                    // Plain `=` on a POINTER dereferences:
                                    // load the target address out of the
                                    // pointer slot, then store through it.
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(b, val, &info.ty);
                                    let tgt = b.load_typed(
                                        info.addr,
                                        IrType::Ptr(Box::new(info.ty.clone())),
                                    );
                                    b.store(coerced, tgt);
                                } else if is_complex_ty(&info.ty) {
                                    // Complex assignment: RHS may evaluate to a
                                    // ptr<[fN x 2]> (already a complex buffer)
                                    // or to a scalar int/real value (Fortran
                                    // permits `c = i` / `c = r` with implicit
                                    // promotion). For the scalar case we have
                                    // to materialize a fresh [fN x 2] buffer
                                    // first — without it we'd memcpy from the
                                    // scalar's value treated as a pointer
                                    // (LAPACK CGEEV's `work(1)=maxwrk` was
                                    // SEGV-ing on this exact path).
                                    let raw = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let src_ty = b.func().value_type(raw);
                                    let dst_fw = complex_float_width(&info.ty);
                                    let src = if matches!(&src_ty, Some(t) if is_complex_ptr_ty(t) && complex_float_width(t) == dst_fw)
                                    {
                                        raw
                                    } else {
                                        materialize_complex_operand(b, raw, dst_fw)
                                    };
                                    let bytes = complex_byte_size(&info.ty);
                                    let sz = b.const_i64(bytes);
                                    if info.by_ref {
                                        let dst = b.load(info.addr);
                                        b.call(
                                            FuncRef::External("memcpy".into()),
                                            vec![dst, src, sz],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                    } else {
                                        b.call(
                                            FuncRef::External("memcpy".into()),
                                            vec![info.addr, src, sz],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                    }
                                } else if is_unlimited_polymorphic_local(&info)
                                    && info.dims.is_empty()
                                    && ctx
                                        .st
                                        .find_symbol_any_scope(&key)
                                        .map(|s| s.attrs.allocatable)
                                        .unwrap_or(false)
                                {
                                    let dst = array_descriptor_addr(b, &info);
                                    let src = class_star_descriptor_source(b, ctx, value);
                                    let owned_bases = b.take_owned_string_temp_bases(src);
                                    copy_unlimited_polymorphic_allocatable_descriptor(
                                        b,
                                        dst,
                                        src,
                                        ctx.type_layouts,
                                    );
                                    deallocate_owned_string_bases(b, &owned_bases);
                                } else if info.by_ref {
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(b, val, &info.ty);
                                    let ptr = b.load(info.addr);
                                    b.store(coerced, ptr);
                                } else {
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(b, val, &info.ty);
                                    b.store(coerced, info.addr);
                                }
                            }
                        }
                    }
                }
                Expr::FunctionCall { callee, args } => {
                    if let Expr::Name { name } = &callee.node {
                        let akey = name.to_lowercase();
                        if let Some(info) = ctx.locals.get(&akey).cloned() {
                            let is_scalar_fixed_alloc_char =
                                local_fixed_char_allocatable_scalar_len(&info).is_some();
                            let has_literal_vector_subscript = args.iter().any(|arg| {
                                matches!(
                                    &arg.value,
                                    crate::ast::expr::SectionSubscript::Element(e)
                                        if matches!(e.node, Expr::ArrayConstructor { .. })
                                )
                            });
                            if local_is_array_like(&info)
                                && !is_scalar_fixed_alloc_char
                                && has_literal_vector_subscript
                                && lower_vector_subscript_section_assign(b, ctx, &info, args, value)
                            {
                                return;
                            }
                            // Vector subscript: a([i1, i2, ...]) = scalar
                            // Expand to scalar assignments a(i1) = scalar, etc.
                            if local_is_array_like(&info)
                                && !is_scalar_fixed_alloc_char
                                && args.len() == 1
                            {
                                if let crate::ast::expr::SectionSubscript::Element(idx_expr) =
                                    &args[0].value
                                {
                                    if let Expr::ArrayConstructor {
                                        values: idx_values, ..
                                    } = &idx_expr.node
                                    {
                                        for v in idx_values {
                                            let crate::ast::expr::AcValue::Expr(scalar_idx) = v
                                            else {
                                                continue;
                                            };
                                            let scalar_target = crate::ast::Spanned::new(
                                                Expr::FunctionCall {
                                                    callee: callee.clone(),
                                                    args: vec![crate::ast::expr::Argument {
                                                        keyword: None,
                                                        value: crate::ast::expr::SectionSubscript::Element(
                                                            scalar_idx.clone(),
                                                        ),
                                                    }],
                                                },
                                                target.span,
                                            );
                                            let scalar_stmt = crate::ast::Spanned::new(
                                                Stmt::Assignment {
                                                    target: scalar_target,
                                                    value: value.clone(),
                                                },
                                                stmt.span,
                                            );
                                            lower_stmt(b, ctx, &scalar_stmt);
                                        }
                                        return;
                                    }
                                }
                            }
                            // Vector subscript with array-returning expression
                            // index: `a(falseloc(mask)) = scalar` etc.  The
                            // subscript is a single Element whose value is an
                            // expression whose result is an integer array.
                            // Materialize that array as a descriptor, then
                            // loop over its elements as scalar indices.
                            if local_is_array_like(&info)
                                && !is_scalar_fixed_alloc_char
                                && args.len() == 1
                                && info.derived_type.is_none()
                                && info.char_kind == CharKind::None
                            {
                                if let crate::ast::expr::SectionSubscript::Element(idx_expr) =
                                    &args[0].value
                                {
                                    if !matches!(idx_expr.node, Expr::ArrayConstructor { .. })
                                        && expr_returns_array(idx_expr, &ctx.locals, ctx.st)
                                        && lower_dynamic_vector_subscript_assign(
                                            b, ctx, &info, idx_expr, value,
                                        )
                                    {
                                        return;
                                    }
                                }
                            }
                            if local_is_array_like(&info)
                                && !is_scalar_fixed_alloc_char
                                && lower_vector_subscript_section_assign(b, ctx, &info, args, value)
                            {
                                return;
                            }
                            if local_is_array_like(&info)
                                && !is_scalar_fixed_alloc_char
                                && is_full_rank1_whole_slice(args)
                            {
                                let whole_view = descriptor_backed_whole_array_view(&info);
                                lower_array_assign(b, ctx, &akey, &whole_view, value);
                                return;
                            }
                            if local_is_array_like(&info)
                                && !is_scalar_fixed_alloc_char
                                && lower_1d_section_assign(b, ctx, &info, args, value)
                            {
                                return;
                            }
                            // Substring LHS: s(lo:hi) = rhs where s is a
                            // scalar character.  Compute the target substring
                            // pointer+length, get the RHS as (ptr, len), and
                            // call afs_assign_char_fixed to do the bounded
                            // copy with space-padding.
                            if (info.char_kind != CharKind::None || is_scalar_fixed_alloc_char)
                                && info.dims.is_empty()
                                && args.len() == 1
                                && matches!(
                                    args[0].value,
                                    crate::ast::expr::SectionSubscript::Range { .. }
                                )
                            {
                                if let crate::ast::expr::SectionSubscript::Range {
                                    ref start,
                                    ref end,
                                    ..
                                } = args[0].value
                                {
                                    if let Some((base_ptr, base_len)) =
                                        char_addr_and_substring_bound_len(
                                            b,
                                            callee,
                                            &ctx.locals,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                        )
                                    {
                                        let (dest_ptr, dest_len) = lower_substring_full(
                                            b,
                                            &ctx.locals,
                                            ctx.st,
                                            base_ptr,
                                            base_len,
                                            start.as_ref(),
                                            end.as_ref(),
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        );
                                        let (src_ptr, src_len) =
                                            lower_string_expr_ctx(b, ctx, value);
                                        b.call(
                                            FuncRef::External("afs_assign_char_fixed".into()),
                                            vec![dest_ptr, dest_len, src_ptr, src_len],
                                            IrType::Void,
                                        );
                                        deallocate_owned_string_expr_temp(
                                            b,
                                            &ctx.locals,
                                            value,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            src_ptr,
                                        );
                                    }
                                }
                            } else if !is_scalar_fixed_alloc_char && local_is_array_like(&info) {
                                // Array element assignment: a(i) = val
                                if info.char_kind != CharKind::None
                                    || descriptor_backed_runtime_char_array(&info)
                                {
                                    lower_char_array_store(
                                        b,
                                        &ctx.locals,
                                        &info,
                                        args,
                                        value,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                    );
                                } else {
                                    if info.derived_type.is_some()
                                        && args.iter().all(|arg| {
                                            matches!(
                                                arg.value,
                                                crate::ast::expr::SectionSubscript::Element(_)
                                            )
                                        })
                                        && try_defined_assignment_for_array_element(
                                            b, ctx, &akey, &info, args, value,
                                        )
                                    {
                                        return;
                                    }
                                    let arr_val = super::expr::lower_expr_ctx(b, ctx, value);
                                    if matches!(
                                        b.func().value_type(arr_val),
                                        Some(IrType::Array(inner, 4096))
                                            if matches!(inner.as_ref(), IrType::Int(IntWidth::I8))
                                    ) && matches!(info.ty, IrType::Ptr(ref inner) if matches!(inner.as_ref(), IrType::Int(IntWidth::I8)))
                                    {
                                        eprintln!(
                                            "DEBUG suspicious array store target={} dims={:?} alloc={} by_ref={} descriptor={} ty={:?}",
                                            name,
                                            info.dims,
                                            info.allocatable,
                                            info.by_ref,
                                            info.descriptor_arg,
                                            info.ty
                                        );
                                    }
                                    lower_array_store(
                                        b,
                                        &ctx.locals,
                                        &info,
                                        args,
                                        arr_val,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                    );
                                }
                            }
                        }
                    } else if let Expr::FunctionCall {
                        callee: inner_callee,
                        args: inner_args,
                    } = &callee.node
                    {
                        if args.len() == 1
                            && matches!(
                                args[0].value,
                                crate::ast::expr::SectionSubscript::Range { .. }
                            )
                        {
                            if let crate::ast::expr::SectionSubscript::Range {
                                ref start,
                                ref end,
                                ..
                            } = args[0].value
                            {
                                if let Expr::Name { name } = &inner_callee.node {
                                    let akey = name.to_lowercase();
                                    if let Some(info) = ctx.locals.get(&akey).cloned() {
                                        if (info.char_kind != CharKind::None
                                            || descriptor_backed_runtime_char_array(&info))
                                            && local_is_array_like(&info)
                                        {
                                            if let Some((elem_ptr, elem_len)) =
                                                char_array_element_ptr_and_len(
                                                    b,
                                                    &ctx.locals,
                                                    &info,
                                                    inner_args,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                )
                                            {
                                                let (dest_ptr, dest_len) = lower_substring_full(
                                                    b,
                                                    &ctx.locals,
                                                    ctx.st,
                                                    elem_ptr,
                                                    elem_len,
                                                    start.as_ref(),
                                                    end.as_ref(),
                                                    Some(ctx.type_layouts),
                                                    Some(ctx.internal_funcs),
                                                    Some(ctx.contained_host_refs),
                                                    Some(ctx.descriptor_params),
                                                );
                                                let (src_ptr, src_len) =
                                                    lower_string_expr_ctx(b, ctx, value);
                                                b.call(
                                                    FuncRef::External(
                                                        "afs_assign_char_fixed".into(),
                                                    ),
                                                    vec![dest_ptr, dest_len, src_ptr, src_len],
                                                    IrType::Void,
                                                );
                                                deallocate_owned_string_expr_temp(
                                                    b,
                                                    &ctx.locals,
                                                    value,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                    src_ptr,
                                                );
                                            }
                                        }
                                    }
                                } else if let Expr::ComponentAccess { .. } = &inner_callee.node {
                                    if let Some((elem_ptr, elem_len)) =
                                        fixed_component_char_array_elem_ptr_and_len(
                                            b,
                                            &ctx.locals,
                                            inner_callee,
                                            inner_args,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    {
                                        let (dest_ptr, dest_len) = lower_substring_full(
                                            b,
                                            &ctx.locals,
                                            ctx.st,
                                            elem_ptr,
                                            elem_len,
                                            start.as_ref(),
                                            end.as_ref(),
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        );
                                        let (src_ptr, src_len) =
                                            lower_string_expr_ctx(b, ctx, value);
                                        b.call(
                                            FuncRef::External("afs_assign_char_fixed".into()),
                                            vec![dest_ptr, dest_len, src_ptr, src_len],
                                            IrType::Void,
                                        );
                                        deallocate_owned_string_expr_temp(
                                            b,
                                            &ctx.locals,
                                            value,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            src_ptr,
                                        );
                                        return;
                                    }
                                    if let Some(info) = component_intrinsic_local_info(
                                        b,
                                        &ctx.locals,
                                        inner_callee,
                                        ctx.st,
                                        ctx.type_layouts,
                                    ) {
                                        if (info.char_kind != CharKind::None
                                            || descriptor_backed_runtime_char_array(&info))
                                            && local_is_array_like(&info)
                                        {
                                            if let Some((elem_ptr, elem_len)) =
                                                char_array_element_ptr_and_len(
                                                    b,
                                                    &ctx.locals,
                                                    &info,
                                                    inner_args,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                )
                                            {
                                                let (dest_ptr, dest_len) = lower_substring_full(
                                                    b,
                                                    &ctx.locals,
                                                    ctx.st,
                                                    elem_ptr,
                                                    elem_len,
                                                    start.as_ref(),
                                                    end.as_ref(),
                                                    Some(ctx.type_layouts),
                                                    Some(ctx.internal_funcs),
                                                    Some(ctx.contained_host_refs),
                                                    Some(ctx.descriptor_params),
                                                );
                                                let (src_ptr, src_len) =
                                                    lower_string_expr_ctx(b, ctx, value);
                                                b.call(
                                                    FuncRef::External(
                                                        "afs_assign_char_fixed".into(),
                                                    ),
                                                    vec![dest_ptr, dest_len, src_ptr, src_len],
                                                    IrType::Void,
                                                );
                                                deallocate_owned_string_expr_temp(
                                                    b,
                                                    &ctx.locals,
                                                    value,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                    src_ptr,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else if let Expr::ComponentAccess { .. } = &callee.node {
                        if let Some((dest_ptr, dest_len)) =
                            fixed_component_char_array_elem_ptr_and_len(
                                b,
                                &ctx.locals,
                                callee,
                                args,
                                ctx.st,
                                ctx.type_layouts,
                            )
                        {
                            let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                            b.call(
                                FuncRef::External("afs_assign_char_fixed".into()),
                                vec![dest_ptr, dest_len, src_ptr, src_len],
                                IrType::Void,
                            );
                            deallocate_owned_string_expr_temp(
                                b,
                                &ctx.locals,
                                value,
                                ctx.st,
                                Some(ctx.type_layouts),
                                src_ptr,
                            );
                            return;
                        }
                        if let Some(info) = component_intrinsic_local_info(
                            b,
                            &ctx.locals,
                            callee,
                            ctx.st,
                            ctx.type_layouts,
                        ) {
                            if local_is_array_like(&info) && is_full_rank1_whole_slice(args) {
                                let whole_view = descriptor_backed_whole_array_view(&info);
                                let alias_name = root_object_name(callee);
                                lower_array_assign(
                                    b,
                                    ctx,
                                    alias_name.as_deref().unwrap_or(""),
                                    &whole_view,
                                    value,
                                );
                                return;
                            }
                            if local_is_array_like(&info)
                                && lower_1d_section_assign(b, ctx, &info, args, value)
                            {
                                return;
                            }
                            if local_is_array_like(&info) {
                                if info.char_kind != CharKind::None
                                    || descriptor_backed_runtime_char_array(&info)
                                {
                                    lower_char_array_store(
                                        b,
                                        &ctx.locals,
                                        &info,
                                        args,
                                        value,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                    );
                                } else {
                                    let arr_val = super::expr::lower_expr_ctx(b, ctx, value);
                                    lower_array_store(
                                        b,
                                        &ctx.locals,
                                        &info,
                                        args,
                                        arr_val,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                    );
                                }
                                return;
                            }
                        }
                        if args.len() == 1
                            && matches!(
                                args[0].value,
                                crate::ast::expr::SectionSubscript::Range { .. }
                            )
                        {
                            if let crate::ast::expr::SectionSubscript::Range {
                                ref start,
                                ref end,
                                ..
                            } = args[0].value
                            {
                                if let Some((field_ptr, field)) = resolve_component_field_access(
                                    b,
                                    &ctx.locals,
                                    callee,
                                    ctx.st,
                                    ctx.type_layouts,
                                ) {
                                    match field_char_kind(&field) {
                                        CharKind::Fixed(flen) => {
                                            let (base_ptr, base_len) =
                                                (field_ptr, b.const_i64(flen));
                                            let (dest_ptr, dest_len) = lower_substring_full(
                                                b,
                                                &ctx.locals,
                                                ctx.st,
                                                base_ptr,
                                                base_len,
                                                start.as_ref(),
                                                end.as_ref(),
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                            );
                                            let (src_ptr, src_len) =
                                                lower_string_expr_ctx(b, ctx, value);
                                            b.call(
                                                FuncRef::External("afs_assign_char_fixed".into()),
                                                vec![dest_ptr, dest_len, src_ptr, src_len],
                                                IrType::Void,
                                            );
                                            deallocate_owned_string_expr_temp(
                                                b,
                                                &ctx.locals,
                                                value,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                src_ptr,
                                            );
                                        }
                                        CharKind::Deferred => {
                                            let (base_ptr, base_len) =
                                                load_string_descriptor_substring_view(b, field_ptr);
                                            let (dest_ptr, dest_len) = lower_substring_full(
                                                b,
                                                &ctx.locals,
                                                ctx.st,
                                                base_ptr,
                                                base_len,
                                                start.as_ref(),
                                                end.as_ref(),
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                            );
                                            let (src_ptr, src_len) =
                                                lower_string_expr_ctx(b, ctx, value);
                                            b.call(
                                                FuncRef::External("afs_assign_char_fixed".into()),
                                                vec![dest_ptr, dest_len, src_ptr, src_len],
                                                IrType::Void,
                                            );
                                            deallocate_owned_string_expr_temp(
                                                b,
                                                &ctx.locals,
                                                value,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                src_ptr,
                                            );
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                Expr::ComponentAccess { base, component } => {
                    if try_lower_complex_part_assignment(b, ctx, base, component, value) {
                        return;
                    }
                    // x%field = val (supports chained: x%a%b = val).
                    if let Some(info) = component_intrinsic_local_info(
                        b,
                        &ctx.locals,
                        target,
                        ctx.st,
                        ctx.type_layouts,
                    ) {
                        if local_is_array_like(&info) {
                            let alias_name = root_object_name(base);
                            lower_array_assign(
                                b,
                                ctx,
                                alias_name.as_deref().unwrap_or(""),
                                &info,
                                value,
                            );
                            return;
                        }
                    }
                    if let Some((base_addr, type_name)) =
                        resolve_component_base(b, &ctx.locals, base, ctx.st, ctx.type_layouts)
                    {
                        if let Some(layout) =
                            type_layout_for_current_scope(ctx.type_layouts, &type_name)
                        {
                            if let Some(field) = layout_component_field_or_parent_view(
                                layout,
                                component,
                                ctx.type_layouts,
                            ) {
                                let offset = b.const_i64(field.offset as i64);
                                let field_ptr =
                                    b.gep(base_addr, vec![offset], IrType::Int(IntWidth::I8));

                                // Character field: copy string data with space padding.
                                if let CharKind::Fixed(flen) = field_char_kind(&field) {
                                    let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                    let dest_ptr =
                                        fixed_char_component_data_ptr(b, field_ptr, &field);
                                    let dest_len = b.const_i64(flen);
                                    b.call(
                                        FuncRef::External("afs_assign_char_fixed".into()),
                                        vec![dest_ptr, dest_len, src_ptr, src_len],
                                        IrType::Void,
                                    );
                                    deallocate_owned_string_expr_temp(
                                        b,
                                        &ctx.locals,
                                        value,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                        src_ptr,
                                    );
                                } else if is_deferred_char_component_field(&field) {
                                    let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                    if field.pointer {
                                        let (dest_ptr, dest_len) =
                                            load_string_descriptor_substring_view(b, field_ptr);
                                        b.call(
                                            FuncRef::External("afs_assign_char_fixed".into()),
                                            vec![dest_ptr, dest_len, src_ptr, src_len],
                                            IrType::Void,
                                        );
                                    } else {
                                        b.call(
                                            FuncRef::External("afs_assign_char_deferred".into()),
                                            vec![field_ptr, src_ptr, src_len],
                                            IrType::Void,
                                        );
                                    }
                                    deallocate_owned_string_expr_temp(
                                        b,
                                        &ctx.locals,
                                        value,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                        src_ptr,
                                    );
                                } else if matches!(
                                    field.type_info,
                                    crate::sema::symtab::TypeInfo::Derived(_)
                                ) && field.allocatable
                                    && field.size == 392
                                    && field.dims.is_empty()
                                {
                                    let Some(type_name) = field_derived_type_name(&field) else {
                                        return;
                                    };
                                    let desc = field_ptr;
                                    let allocated = b.call(
                                        FuncRef::External("afs_allocated".into()),
                                        vec![desc],
                                        IrType::Int(IntWidth::I32),
                                    );
                                    let zero32 = b.const_i32(0);
                                    let needs_alloc = b.icmp(CmpOp::Eq, allocated, zero32);
                                    let alloc_bb =
                                        b.create_block("component_scalar_derived_assign_alloc");
                                    let copy_bb =
                                        b.create_block("component_scalar_derived_assign_copy");
                                    let done_bb =
                                        b.create_block("component_scalar_derived_assign_done");
                                    b.cond_branch(needs_alloc, alloc_bb, vec![], copy_bb, vec![]);

                                    b.set_block(alloc_bb);
                                    if let Some(layout) =
                                        type_layout_for_current_scope(ctx.type_layouts, &type_name)
                                    {
                                        let elem_size = b.const_i64(layout.size as i64);
                                        let rank_val = b.const_i32(0);
                                        let null_ptr = b.const_i64(0);
                                        b.call(
                                            FuncRef::External("afs_allocate_array".into()),
                                            vec![desc, elem_size, rank_val, null_ptr, null_ptr],
                                            IrType::Void,
                                        );
                                        if derived_layout_needs_runtime_initialization(
                                            layout,
                                            ctx.type_layouts,
                                        ) {
                                            let base_ptr = b.load_typed(
                                                desc,
                                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                            );
                                            initialize_derived_storage(
                                                b,
                                                base_ptr,
                                                layout,
                                                ctx.type_layouts,
                                            );
                                        }
                                    }
                                    b.branch(copy_bb, vec![]);

                                    b.set_block(copy_bb);
                                    let src_ptr =
                                        scalar_allocatable_derived_component_payload_addr(
                                            b,
                                            &ctx.locals,
                                            value,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                        .unwrap_or_else(
                                            || super::expr::lower_expr_ctx_tl(b, ctx, value),
                                        );
                                    let dest_ptr = b.load_typed(
                                        desc,
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                    emit_derived_value_copy(
                                        b,
                                        ctx.type_layouts,
                                        &type_name,
                                        dest_ptr,
                                        src_ptr,
                                    );
                                    if let Some(tag) = derived_type_tag_value(
                                        b,
                                        Some(type_name.as_str()),
                                        ctx.type_layouts,
                                    ) {
                                        store_array_desc_type_tag(b, desc, tag);
                                    }
                                    if let Some(lookup) = derived_type_vtable_value(
                                        b,
                                        Some(type_name.as_str()),
                                        ctx.type_layouts,
                                    ) {
                                        store_array_desc_vtable_ptr(b, desc, lookup);
                                    }
                                    b.branch(done_bb, vec![]);
                                    b.set_block(done_bb);
                                } else if matches!(
                                    field.type_info,
                                    crate::sema::symtab::TypeInfo::Derived(_)
                                ) && !is_opaque_c_handle_type(&field.type_info)
                                    && !field.pointer
                                    && !field.allocatable
                                    && field.dims.is_empty()
                                {
                                    let result_temp_type =
                                        finalizable_function_result_type_name(ctx, value);
                                    let src_ptr = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    if let Some(nested_name) = field_derived_type_name(&field) {
                                        let src_ptr = stabilize_finalized_assignment_rhs(
                                            b,
                                            ctx,
                                            &nested_name,
                                            src_ptr,
                                        );
                                        finalize_assignment_lhs(b, ctx, &nested_name, field_ptr);
                                        emit_derived_value_copy(
                                            b,
                                            ctx.type_layouts,
                                            &nested_name,
                                            field_ptr,
                                            src_ptr,
                                        );
                                        if let Some(ref temp_type) = result_temp_type {
                                            finalize_assignment_lhs(b, ctx, temp_type, src_ptr);
                                        }
                                    }
                                } else if is_complex_ty(&type_info_to_ir_type(&field.type_info))
                                    && !field.pointer
                                    && !field.allocatable
                                    && field.dims.is_empty()
                                {
                                    let raw = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let field_ir_ty = type_info_to_ir_type(&field.type_info);
                                    let src_ty = b.func().value_type(raw);
                                    let src = if matches!(&src_ty, Some(t) if is_complex_ptr_ty(t))
                                    {
                                        raw
                                    } else {
                                        let fw = complex_float_width(&field_ir_ty);
                                        materialize_complex_operand(b, raw, fw)
                                    };
                                    let bytes = complex_byte_size(&field_ir_ty);
                                    let sz = b.const_i64(bytes);
                                    b.call(
                                        FuncRef::External("memcpy".into()),
                                        vec![field_ptr, src, sz],
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                } else if field.pointer
                                    && !field.procedure_pointer
                                    && !field.allocatable
                                    && field.dims.is_empty()
                                    && !matches!(
                                        field.type_info,
                                        crate::sema::symtab::TypeInfo::ClassStar
                                            | crate::sema::symtab::TypeInfo::TypeStar
                                    )
                                {
                                    if matches!(
                                        field.type_info,
                                        crate::sema::symtab::TypeInfo::Derived(_)
                                    ) && !is_opaque_c_handle_type(&field.type_info)
                                    {
                                        let src_ptr = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                        if let Some(nested_name) = field_derived_type_name(&field) {
                                            let dest_ptr = b.load_typed(
                                                field_ptr,
                                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                            );
                                            emit_derived_value_copy(
                                                b,
                                                ctx.type_layouts,
                                                &nested_name,
                                                dest_ptr,
                                                src_ptr,
                                            );
                                        }
                                    } else if is_complex_ty(&type_info_to_ir_type(&field.type_info))
                                    {
                                        let raw = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                        let field_ir_ty = type_info_to_ir_type(&field.type_info);
                                        let src_ty = b.func().value_type(raw);
                                        let src = if matches!(&src_ty, Some(t) if is_complex_ptr_ty(t))
                                        {
                                            raw
                                        } else {
                                            let fw = complex_float_width(&field_ir_ty);
                                            materialize_complex_operand(b, raw, fw)
                                        };
                                        let dest_ptr = b.load_typed(
                                            field_ptr,
                                            IrType::Ptr(Box::new(field_storage_ir_type(
                                                &field,
                                                ctx.type_layouts,
                                            ))),
                                        );
                                        let bytes = complex_byte_size(&field_ir_ty);
                                        let sz = b.const_i64(bytes);
                                        b.call(
                                            FuncRef::External("memcpy".into()),
                                            vec![dest_ptr, src, sz],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                    } else {
                                        let elem_ty = type_info_to_ir_type(&field.type_info);
                                        let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                        let coerced = coerce_to_type(b, val, &elem_ty);
                                        let dest_ptr =
                                            b.load_typed(field_ptr, IrType::Ptr(Box::new(elem_ty)));
                                        b.store(coerced, dest_ptr);
                                    }
                                } else if matches!(
                                    field.type_info,
                                    crate::sema::symtab::TypeInfo::ClassStar
                                        | crate::sema::symtab::TypeInfo::TypeStar
                                ) && field.allocatable
                                    && field.dims.is_empty()
                                {
                                    let src = class_star_descriptor_source(b, ctx, value);
                                    let owned_bases = b.take_owned_string_temp_bases(src);
                                    copy_unlimited_polymorphic_allocatable_descriptor(
                                        b,
                                        field_ptr,
                                        src,
                                        ctx.type_layouts,
                                    );
                                    deallocate_owned_string_bases(b, &owned_bases);
                                } else if field.allocatable
                                    && !field.pointer
                                    && !field.declared_array
                                    && field.dims.is_empty()
                                    && field.size == 392
                                {
                                    let desc = field_ptr;
                                    let allocated = b.call(
                                        FuncRef::External("afs_allocated".into()),
                                        vec![desc],
                                        IrType::Int(IntWidth::I32),
                                    );
                                    let zero32 = b.const_i32(0);
                                    let needs_alloc = b.icmp(CmpOp::Eq, allocated, zero32);
                                    let alloc_bb =
                                        b.create_block("component_scalar_intrinsic_assign_alloc");
                                    let store_bb =
                                        b.create_block("component_scalar_intrinsic_assign_store");
                                    let done_bb =
                                        b.create_block("component_scalar_intrinsic_assign_done");
                                    b.cond_branch(needs_alloc, alloc_bb, vec![], store_bb, vec![]);

                                    b.set_block(alloc_bb);
                                    let elem_ty = type_info_to_ir_type(&field.type_info);
                                    let elem_size =
                                        b.const_i64(ir_scalar_byte_size(&elem_ty, b.layout));
                                    let rank_val = b.const_i32(0);
                                    let null_ptr = b.const_i64(0);
                                    b.call(
                                        FuncRef::External("afs_allocate_array".into()),
                                        vec![desc, elem_size, rank_val, null_ptr, null_ptr],
                                        IrType::Void,
                                    );
                                    b.branch(store_bb, vec![]);

                                    b.set_block(store_bb);
                                    let elem_ty = type_info_to_ir_type(&field.type_info);
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(b, val, &elem_ty);
                                    let dest_ptr =
                                        b.load_typed(desc, IrType::Ptr(Box::new(elem_ty)));
                                    b.store(coerced, dest_ptr);
                                    b.branch(done_bb, vec![]);

                                    b.set_block(done_bb);
                                } else {
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let coerced = coerce_to_type(
                                        b,
                                        val,
                                        &type_info_to_ir_type(&field.type_info),
                                    );
                                    b.store(coerced, field_ptr);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Stmt::Print { format, items } => {
            // PRINT writes to unit 6 (stdout). `PRINT *` is list-directed;
            // `PRINT fmt` with a character format string or labeled FORMAT
            // statement routes through the same push-based formatted machinery
            // WRITE uses.
            let unit = b.const_i32(6);
            // The push-based formatted runtime is re-entrant: FMT_CTX is a
            // STACK of contexts (begin pushes, end pops), so an output item
            // whose evaluation performs nested I/O (a function doing an
            // internal `write(str,...)`) runs in its own context and leaves
            // the outer one intact. The old single-global engine forced a
            // list-directed fallback whenever an item contained a procedure
            // call, which silently ignored the format — `print '(a,f8.3)',
            // 'area = ', area(r)` printed E-notation in every real program
            // that prints a computed value.
            let is_formatted = is_formatted_format_expr(ctx, format);
            if is_formatted {
                let null_i64 = b.const_i64(0);
                let null_i8_ptr = b.int_to_ptr(null_i64, IrType::Int(IntWidth::I8));
                let zero_i64 = b.const_i64(0);
                let (fmt_ptr, fmt_len) = lower_format_expr(b, ctx, format);
                b.call(
                    FuncRef::External("afs_fmt_begin_ex".into()),
                    vec![unit, fmt_ptr, fmt_len, null_i8_ptr, null_i8_ptr, zero_i64],
                    IrType::Void,
                );
                for item in items {
                    lower_fmt_push(b, ctx, item);
                }
                let adv = b.const_i32(1);
                b.call(
                    FuncRef::External("afs_fmt_end".into()),
                    vec![adv],
                    IrType::Void,
                );
                deallocate_owned_string_expr_temp(
                    b,
                    &ctx.locals,
                    format,
                    ctx.st,
                    Some(ctx.type_layouts),
                    fmt_ptr,
                );
            } else {
                lower_write_items(b, ctx, items, unit);
            }
        }

        Stmt::Write { controls, items } => {
            // Check for format specifier (second positional control).
            // * means list-directed; a string literal means formatted.
            let fmt_control = controls
                .iter()
                .skip(1)
                .find(|c| c.keyword.is_none()) // positional, not keyword=
                .or_else(|| {
                    controls.iter().find(|c| {
                        c.keyword
                            .as_deref()
                            .map(|k| k.eq_ignore_ascii_case("fmt"))
                            .unwrap_or(false)
                    })
                });

            let is_list_directed = match fmt_control {
                None => true,
                Some(ctrl) => matches!(&ctrl.value.node, Expr::Name { name } if name == "*"),
            };

            // Check for ADVANCE='NO'.
            //
            // `advance_static` is the compile-time bool used by the
            // existing per-item helpers. When `advance=` is a string
            // literal we honor it directly. When it's a non-literal
            // expression (e.g. `advance=optval(adv, 'YES')` from
            // stdlib's write_bitset_unit_64), we cannot decide at
            // compile time, so we keep `advance_static = true`
            // (preserve item lowering's optional newline emit) and
            // separately compute `advance_runtime`, an i32 value the
            // runtime helpers consult to suppress the newline when the
            // expression evaluates to "no" at runtime.
            let advance_ctrl = controls.iter().find(|c| {
                c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("advance"))
                    .unwrap_or(false)
            });
            let advance_static = advance_ctrl
                .map(|c| {
                    if let Expr::StringLiteral { value, .. } = &c.value.node {
                        !value.eq_ignore_ascii_case("no")
                    } else {
                        true
                    }
                })
                .unwrap_or(true);
            let advance_runtime: Option<ValueId> = advance_ctrl.and_then(|c| {
                if matches!(&c.value.node, Expr::StringLiteral { .. }) {
                    None
                } else {
                    let (p, l) = lower_string_expr_ctx(b, ctx, &c.value);
                    let result = b.call(
                        FuncRef::External("afs_advance_eval".into()),
                        vec![p, l],
                        IrType::Int(IntWidth::I32),
                    );
                    deallocate_owned_string_expr_temp(
                        b,
                        &ctx.locals,
                        &c.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                        p,
                    );
                    Some(result)
                }
            });
            let advance = advance_static;

            // Optional iostat=ios / iomsg=msg specifiers. The push-based
            // formatted runtime ignored these on previous builds, so a
            // caller's `if (ios /= 0) error_stop` always tripped on the
            // pre-call sentinel — stdlib's savetxt loops exactly that
            // pattern around `write(unit, fmt_, iostat=ios) d(i, :)` and
            // unconditionally error_stops every example_savetxt /
            // example_loadtxt without proper iostat plumbing.
            let null_i64 = b.const_i64(0);
            let null_i8_ptr = b.int_to_ptr(null_i64, IrType::Int(IntWidth::I8));
            let zero_i64 = b.const_i64(0);
            let iostat_ctrl = controls.iter().find(|c| {
                c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("iostat"))
                    .unwrap_or(false)
            });
            let iomsg_ctrl = controls.iter().find(|c| {
                c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("iomsg"))
                    .unwrap_or(false)
            });
            // LEADING_ZERO= statement override (F2023): seeds the format
            // engine's leading-zero mode for this WRITE, beating the
            // connection mode. Only meaningful for formatted output.
            let leading_zero_ctrl = controls.iter().find(|c| {
                c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("leading_zero"))
                    .unwrap_or(false)
            });
            let iostat_arg_ptr = iostat_ctrl.map(|c| lower_arg_by_ref_ctx(b, ctx, &c.value));
            let iostat_ptr = iostat_arg_ptr.unwrap_or(null_i8_ptr);
            let (iomsg_arg_ptr, iomsg_ptr, iomsg_len) = if let Some(c) = iomsg_ctrl {
                let arg_ptr = lower_arg_by_ref_ctx(b, ctx, &c.value);
                let (ptr, len) = lower_string_expr_ctx(b, ctx, &c.value);
                (arg_ptr, ptr, len)
            } else {
                (null_char_slot_arg(b), null_i8_ptr, zero_i64)
            };

            if lower_namelist_write_stmt(b, ctx, controls, iostat_ptr) {
                return;
            }

            if let Some(ctrl) = controls.first() {
                // Internal WRITE to a deferred-length allocatable
                // `character(:), allocatable` scalar: the target is
                // reallocated to the record length (F2023 §12.4), whether it
                // was unallocated or already allocated. Formatted goes through
                // the fmt engine's InternalAlloc sink; list-directed collects
                // the record in the runtime (afs_lst_ia_*) and stores it in one
                // shot with the same semantics.
                if let Some(desc) = internal_io_alloc_target(b, ctx, ctrl) {
                    if is_list_directed {
                        b.call(
                            FuncRef::External("afs_lst_ia_begin".into()),
                            vec![desc, iostat_ptr, iomsg_ptr, iomsg_len],
                            IrType::Void,
                        );
                        lower_internal_write_items_alloc(b, ctx, items);
                        b.call(
                            FuncRef::External("afs_lst_ia_end".into()),
                            vec![],
                            IrType::Void,
                        );
                        return;
                    }
                    let (fmt_ptr, fmt_len) = lower_format_expr(b, ctx, &fmt_control.unwrap().value);
                    b.call(
                        FuncRef::External("afs_fmt_begin_internal_alloc".into()),
                        vec![desc, fmt_ptr, fmt_len, iostat_ptr, iomsg_ptr, iomsg_len],
                        IrType::Void,
                    );
                    lower_fmt_leading_zero_override(b, ctx, leading_zero_ctrl);
                    for item in items {
                        lower_fmt_push(b, ctx, item);
                    }
                    let adv =
                        advance_runtime.unwrap_or_else(|| b.const_i32(if advance { 1 } else { 0 }));
                    b.call(
                        FuncRef::External("afs_fmt_end".into()),
                        vec![adv],
                        IrType::Void,
                    );
                    deallocate_owned_string_expr_temp(
                        b,
                        &ctx.locals,
                        &fmt_control.unwrap().value,
                        ctx.st,
                        Some(ctx.type_layouts),
                        fmt_ptr,
                    );
                    return;
                }
                // Whole character array as the internal unit: formatted
                // writes get record-per-element semantics; unallocated or
                // overflowed targets error loudly in the runtime. Our
                // list-directed processor emits a single record, which
                // goes into element one (previously both shapes silently
                // wrote nothing through a zero-length buffer view).
                if let Some((base, elem_len, nelems)) = internal_io_array_target(b, ctx, ctrl) {
                    if is_list_directed {
                        lower_internal_write_items(b, ctx, items, base, elem_len);
                        return;
                    }
                    let (fmt_ptr, fmt_len) = lower_format_expr(b, ctx, &fmt_control.unwrap().value);
                    b.call(
                        FuncRef::External("afs_fmt_begin_internal_array".into()),
                        vec![
                            base, elem_len, nelems, fmt_ptr, fmt_len, iostat_ptr, iomsg_ptr,
                            iomsg_len,
                        ],
                        IrType::Void,
                    );
                    lower_fmt_leading_zero_override(b, ctx, leading_zero_ctrl);
                    for item in items {
                        lower_fmt_push(b, ctx, item);
                    }
                    let adv =
                        advance_runtime.unwrap_or_else(|| b.const_i32(if advance { 1 } else { 0 }));
                    b.call(
                        FuncRef::External("afs_fmt_end".into()),
                        vec![adv],
                        IrType::Void,
                    );
                    deallocate_owned_string_expr_temp(
                        b,
                        &ctx.locals,
                        &fmt_control.unwrap().value,
                        ctx.st,
                        Some(ctx.type_layouts),
                        fmt_ptr,
                    );
                    return;
                }
                if let Some((buf_ptr, buf_len)) = internal_io_buffer(b, ctx, ctrl) {
                    if is_list_directed {
                        lower_internal_write_items(b, ctx, items, buf_ptr, buf_len);
                    } else {
                        let (fmt_ptr, fmt_len) =
                            lower_format_expr(b, ctx, &fmt_control.unwrap().value);
                        b.call(
                            FuncRef::External("afs_fmt_begin_internal_ex".into()),
                            vec![
                                buf_ptr, buf_len, fmt_ptr, fmt_len, iostat_ptr, iomsg_ptr,
                                iomsg_len,
                            ],
                            IrType::Void,
                        );
                        lower_fmt_leading_zero_override(b, ctx, leading_zero_ctrl);
                        for item in items {
                            lower_fmt_push(b, ctx, item);
                        }
                        let adv = advance_runtime
                            .unwrap_or_else(|| b.const_i32(if advance { 1 } else { 0 }));
                        b.call(
                            FuncRef::External("afs_fmt_end".into()),
                            vec![adv],
                            IrType::Void,
                        );
                        deallocate_owned_string_expr_temp(
                            b,
                            &ctx.locals,
                            &fmt_control.unwrap().value,
                            ctx.st,
                            Some(ctx.type_layouts),
                            fmt_ptr,
                        );
                    }
                    return;
                }
            }

            // Extract unit (first control). * means stdout (unit 6).
            let unit = if let Some(ctrl) = controls.first() {
                if matches!(&ctrl.value.node, Expr::Name { name } if name == "*") {
                    b.const_i32(6)
                } else {
                    super::expr::lower_expr_ctx(b, ctx, &ctrl.value)
                }
            } else {
                b.const_i32(6)
            };

            lower_external_io_pos_seek(b, ctx, controls, unit, iostat_ptr);

            let defined_iotype = match fmt_control {
                Some(ctrl) if matches!(&ctrl.value.node, Expr::Name { name } if name == "*") => {
                    Some("LISTDIRECTED")
                }
                Some(_) => Some("DT"),
                None => None,
            };
            if try_lower_defined_io_write_items(
                b,
                ctx,
                items,
                unit,
                defined_iotype,
                iostat_arg_ptr,
                iomsg_arg_ptr,
                iomsg_len,
            ) {
                return;
            }

            if is_list_directed {
                // Wrap the per-item writes in begin/end so the runtime
                // can (a) emit sequential-unformatted record markers,
                // and (b) thread iostat=/iomsg= through. Pass
                // `advance=false` to suppress the per-item helper's
                // unconditional newline emit; we emit our own
                // `afs_write_newline_if` afterwards using the i32 that
                // honors a runtime-evaluated advance= expression
                // (e.g. `advance=optval(adv,'YES')`).
                b.call(
                    FuncRef::External("afs_list_write_begin".into()),
                    vec![unit, iostat_ptr, iomsg_ptr, iomsg_len],
                    IrType::Void,
                );
                lower_write_items_adv(b, ctx, items, unit, false);
                let adv =
                    advance_runtime.unwrap_or_else(|| b.const_i32(if advance { 1 } else { 0 }));
                b.call(
                    FuncRef::External("afs_write_newline_if".into()),
                    vec![unit, adv],
                    IrType::Void,
                );
                b.call(
                    FuncRef::External("afs_list_write_end".into()),
                    vec![unit, adv, iostat_ptr, iomsg_ptr, iomsg_len],
                    IrType::Void,
                );
            } else {
                // Formatted I/O: use push-based API.
                let (fmt_ptr, fmt_len) = lower_format_expr(b, ctx, &fmt_control.unwrap().value);
                b.call(
                    FuncRef::External("afs_fmt_begin_ex".into()),
                    vec![unit, fmt_ptr, fmt_len, iostat_ptr, iomsg_ptr, iomsg_len],
                    IrType::Void,
                );
                lower_fmt_leading_zero_override(b, ctx, leading_zero_ctrl);

                for item in items {
                    lower_fmt_push(b, ctx, item);
                }

                let adv =
                    advance_runtime.unwrap_or_else(|| b.const_i32(if advance { 1 } else { 0 }));
                b.call(
                    FuncRef::External("afs_fmt_end".into()),
                    vec![adv],
                    IrType::Void,
                );
                deallocate_owned_string_expr_temp(
                    b,
                    &ctx.locals,
                    &fmt_control.unwrap().value,
                    ctx.st,
                    Some(ctx.type_layouts),
                    fmt_ptr,
                );
            }
        }

        Stmt::Call { callee, args } => {
            // Handle type-bound procedure calls: call obj%method(args)
            if let Expr::ComponentAccess { base, component } = &callee.node {
                if emit_polymorphic_component_bound_dispatch(
                    b,
                    &ctx.locals,
                    ctx.st,
                    Some(ctx.type_layouts),
                    Some(ctx.internal_funcs),
                    Some(ctx.contained_host_refs),
                    Some(ctx.optional_params),
                    Some(ctx.descriptor_params),
                    Some(ctx.char_len_star_params),
                    callee.span,
                    base,
                    component,
                    args,
                    Some(IrType::Void),
                )
                .is_some()
                {
                    return;
                }
                if let Some((obj_addr, type_name)) = resolve_component_base_for_method(
                    b,
                    &ctx.locals,
                    base,
                    ctx.st,
                    ctx.type_layouts,
                ) {
                    if let Some(layout) = ctx.type_layouts.get(&type_name) {
                        let candidates = layout.bound_proc_candidates(component);
                        if !candidates.is_empty() {
                            let pass_desc_addr = lower_arg_descriptor(
                                b,
                                &ctx.locals,
                                base,
                                ctx.st,
                                Some(ctx.type_layouts),
                                false,
                            );
                            let bp = resolved_bound_proc_for_call(
                                b,
                                &ctx.locals,
                                ctx.st,
                                layout,
                                component,
                                args,
                                Some(ctx.type_layouts),
                                Some(ctx.internal_funcs),
                                Some(ctx.contained_host_refs),
                                Some(ctx.descriptor_params),
                            )
                            .or_else(|| layout.bound_proc(component))
                            .unwrap_or_else(|| {
                                fail_unmatched_bound_proc_resolution(callee.span, layout, component)
                            });
                            let _ = emit_resolved_bound_proc_call(
                                b,
                                &ctx.locals,
                                ctx.st,
                                Some(ctx.type_layouts),
                                Some(ctx.internal_funcs),
                                Some(ctx.contained_host_refs),
                                Some(ctx.optional_params),
                                Some(ctx.descriptor_params),
                                Some(ctx.char_len_star_params),
                                obj_addr,
                                Some(pass_desc_addr),
                                FuncRef::External(bp.target_name.clone()),
                                bp,
                                args,
                                None,
                                IrType::Void,
                            );
                            return;
                        }
                    }
                }
                if let Some((target, closure_args, signature_key, procptr_nopass)) =
                    procedure_pointer_component_call_target(
                        b,
                        &ctx.locals,
                        callee,
                        ctx.st,
                        ctx.type_layouts,
                    )
                {
                    let formal_skip = if procptr_nopass { 0 } else { 1 };
                    let arg_slots = reorder_args_by_keyword_slots_with_formal_skip(
                        args,
                        &signature_key,
                        ctx.st,
                        formal_skip,
                    );
                    let abi_lookup_keys = procedure_abi_lookup_keys(ctx.st, &[&signature_key]);
                    let abi_primary_key = abi_lookup_keys
                        .first()
                        .map(String::as_str)
                        .unwrap_or(signature_key.as_str());
                    let value_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_value_arg_mask(ctx.st, k)
                    });
                    let desc_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        descriptor_param_mask_for_lookup(ctx.st, ctx.descriptor_params, k)
                    });
                    let bind_c_char_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_bind_c_char_arg_mask(ctx.st, k)
                    });
                    let char_len_star_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.char_len_star_params, k)
                            .or_else(|| callee_char_len_star_mask(ctx.st, k))
                    });
                    let pointer_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_pointer_arg_mask(ctx.st, k)
                    });
                    let allocatable_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_allocatable_arg_mask(ctx.st, k)
                    });
                    let class_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_class_arg_mask(ctx.st, k)
                    });
                    let string_desc_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_string_descriptor_arg_mask(ctx.st, k)
                    });
                    let opt_flags = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.optional_params, k)
                            .or_else(|| callee_optional_arg_mask(ctx.st, k))
                    });
                    let sequence_array_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_sequence_array_arg_mask(ctx.st, k)
                    });
                    let sequence_array_copy_back_mask =
                        first_procedure_lookup(&abi_lookup_keys, |k| {
                            callee_sequence_array_copy_back_mask(ctx.st, k)
                        });
                    let mut call_arg_sequence_temps = Vec::new();
                    let mut arg_vals: Vec<ValueId> = Vec::with_capacity(arg_slots.len());
                    let mut arg_presence: Vec<ValueId> = Vec::with_capacity(arg_slots.len());
                    let mut call_arg_array_temps = Vec::new();
                    let mut call_arg_character_lens = vec![None; arg_slots.len()];
                    let mut call_arg_character_temps = Vec::new();
                    for (i, slot) in arg_slots.iter().enumerate() {
                        let mask_wants_descriptor = desc_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_bind_c_char = bind_c_char_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let expects_char_len_star = char_len_star_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        if !procptr_nopass && i == 0 {
                            arg_vals.push(if mask_wants_descriptor && !wants_bind_c_char {
                                lower_arg_descriptor_full(
                                    b,
                                    &ctx.locals,
                                    base,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                    Some(ctx.internal_funcs),
                                    Some(ctx.contained_host_refs),
                                    Some(ctx.descriptor_params),
                                    false,
                                )
                            } else {
                                let dummy_is_class = class_mask
                                    .as_ref()
                                    .map(|mask| mask.get(i).copied().unwrap_or(false))
                                    .unwrap_or(false);
                                lower_arg_by_ref_for_dummy_full(
                                    b,
                                    &ctx.locals,
                                    base,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                    Some(ctx.internal_funcs),
                                    Some(ctx.contained_host_refs),
                                    Some(ctx.descriptor_params),
                                    dummy_is_class,
                                )
                            });
                            arg_presence.push(b.const_bool(true));
                            continue;
                        }
                        let is_value = value_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_pointer = pointer_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let dummy_is_allocatable = allocatable_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_string_descriptor = string_desc_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_sequence_array = sequence_array_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let sequence_array_copy_back = sequence_array_copy_back_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let is_optional = opt_flags
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let dummy_is_class = mask_wants_descriptor
                            && class_mask
                                .as_ref()
                                .map(|mask| mask.get(i).copied().unwrap_or(false))
                                .unwrap_or(false);
                        let wants_polymorphic_descriptor =
                            dummy_is_class && !dummy_is_allocatable && !wants_pointer;
                        let wants_string_descriptor = wants_string_descriptor && !wants_bind_c_char;
                        let lowered = match slot {
                            Some(arg) => {
                                match &arg.value {
                                    crate::ast::expr::SectionSubscript::Element(arg_expr) => {
                                        let actual_is_array_section =
                                            actual_is_array_section_designator(
                                                &ctx.locals,
                                                arg_expr,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            );
                                        let actual_is_array = actual_expr_rank(
                                            arg_expr,
                                            &ctx.locals,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                        )
                                        .is_some_and(|rank| rank > 0)
                                            || actual_is_array_section;
                                        let actual_is_char_sequence =
                                            actual_is_character_array_section_designator(
                                                &ctx.locals,
                                                arg_expr,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            );
                                        let sequence_array_for_arg = (wants_sequence_array
                                            || actual_is_array)
                                            && !matches!(
                                                arg_expr.node,
                                                Expr::ConditionalExpr { .. } | Expr::NilArgument
                                            );
                                        let sequence_array_copy_back_for_arg =
                                            if wants_sequence_array {
                                                sequence_array_copy_back
                                            } else {
                                                true
                                            };
                                        // Same conditional-argument treatment as
                                        // the direct-call path; wants_descriptor
                                        // is per-arm (it inspects the actual).
                                        let mut materialize = |b: &mut FuncBuilder,
                                                               e: &crate::ast::expr::SpannedExpr|
                                         -> MaterializedCallArg {
                                    let mut character_len = None;
                                    let mut owned_character_bases = Vec::new();
                                    let wants_descriptor = (mask_wants_descriptor
                                        || (desc_mask.is_none()
                                            && actual_is_descriptor_backed(
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            )))
                                        && !wants_bind_c_char;
                                    let value = if wants_bind_c_char {
                                        let actual = lower_bind_c_char_call_arg(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                            is_value,
                                        );
                                        character_len = actual.character_len;
                                        owned_character_bases = actual.owned_character_bases;
                                        actual.value
                                    } else if is_value {
                                        let raw = super::expr::lower_expr_full(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        );
                                        coerce_value_call_arg(b, ctx.st, abi_primary_key, i, raw)
                                    } else if wants_descriptor {
                                        let desc = lower_arg_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            wants_polymorphic_descriptor,
                                        );
                                        owned_character_bases
                                            .extend(b.take_owned_string_temp_bases(desc));
                                        desc
                                    } else if wants_string_descriptor {
                                        lower_arg_string_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                        )
                                    } else if wants_pointer {
                                        lower_pointer_dummy_actual(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        )
                                        .unwrap_or_else(|| {
                                            lower_arg_by_ref_for_dummy_full(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                dummy_is_class,
                                            )
                                        })
                                    } else if sequence_array_for_arg {
                                        let sequence_actual = if actual_is_char_sequence {
                                            lower_sequence_char_array_actual(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                sequence_array_copy_back_for_arg,
                                                &mut call_arg_sequence_temps,
                                            )
                                        } else {
                                            lower_sequence_array_actual(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                sequence_array_copy_back_for_arg,
                                                &mut call_arg_sequence_temps,
                                            )
                                        };
                                        sequence_actual
                                        .unwrap_or_else(|| {
                                            lower_arg_by_ref_for_dummy_full(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                dummy_is_class,
                                            )
                                        })
                                    } else {
                                        if let Some(actual) = lower_materialized_character_actual(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        ) {
                                            character_len = Some(actual.len);
                                            owned_character_bases = actual.owned_bases;
                                            actual.address
                                        } else {
                                            lower_arg_by_ref_for_dummy_full(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                dummy_is_class,
                                            )
                                        }
                                    };
                                    let value = optional_arg_absent_if_forwarded_by_ref_dummy(
                                        b,
                                        &ctx.locals,
                                        e,
                                        is_optional && !is_value,
                                        value,
                                    );
                                    let value = optional_arg_absent_if_unallocated_allocatable(
                                        b,
                                        &ctx.locals,
                                        e,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                        is_optional
                                            && !is_value
                                            && !dummy_is_allocatable
                                            && !wants_bind_c_char
                                            && !wants_pointer,
                                        value,
                                    );
                                    if expects_char_len_star && character_len.is_none() {
                                        character_len = actual_char_arg_runtime_len(
                                            b,
                                            &ctx.locals,
                                            Some(&ctx.optional_locals),
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        );
                                    }
                                    MaterializedCallArg {
                                        value,
                                        character_len,
                                        owned_character_bases,
                                    }
                                    };
                                        let lowered = lower_call_arg_maybe_conditional(
                                            b,
                                            &ctx.locals,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                            arg_expr,
                                            abi_primary_key,
                                            i,
                                            is_value,
                                            &mut materialize,
                                        );
                                        call_arg_character_lens[i] = lowered.character_len;
                                        call_arg_character_temps
                                            .extend(lowered.owned_character_bases.iter().copied());
                                        let wants_descriptor = (mask_wants_descriptor
                                            || (desc_mask.is_none()
                                                && actual_is_descriptor_backed(
                                                    &ctx.locals,
                                                    arg_expr,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                )))
                                            && !wants_bind_c_char;
                                        if wants_descriptor
                                            && !matches!(
                                                arg_expr.node,
                                                Expr::ConditionalExpr { .. } | Expr::NilArgument
                                            )
                                        {
                                            track_call_arg_array_temp_descriptor(
                                                b,
                                                &mut call_arg_array_temps,
                                                &ctx.locals,
                                                arg_expr,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                lowered.value,
                                            );
                                        }
                                        lowered
                                    }
                                    _ => {
                                        let value = b.const_i32(0);
                                        let present = b.const_bool(true);
                                        LoweredCallArg::plain(value, present)
                                    }
                                }
                            }
                            None => {
                                let value = missing_optional_call_arg(
                                    b,
                                    ctx.st,
                                    abi_primary_key,
                                    i,
                                    is_value,
                                );
                                let present = b.const_bool(false);
                                LoweredCallArg::plain(value, present)
                            }
                        };
                        arg_vals.push(lowered.value);
                        arg_presence.push(lowered.present);
                    }
                    if let Some(opt_flags) = &opt_flags {
                        let missing_slots = arg_slots.len().saturating_sub(arg_vals.len());
                        for flag in opt_flags.iter().skip(arg_vals.len()).take(missing_slots) {
                            if *flag {
                                arg_vals.push(b.const_i64(0));
                            }
                        }
                    }
                    append_optional_value_presence_args(
                        b,
                        opt_flags.as_deref(),
                        value_mask.as_deref(),
                        &arg_presence,
                        &mut arg_vals,
                    );
                    if let Some(cls_flags) = &char_len_star_mask {
                        for (i, flag) in cls_flags.iter().enumerate() {
                            if !*flag || i >= arg_slots.len() {
                                continue;
                            }
                            if let Some(arg) = &arg_slots[i] {
                                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                                    let len = call_arg_character_lens[i].or_else(|| {
                                        actual_char_arg_runtime_len(
                                            b,
                                            &ctx.locals,
                                            Some(&ctx.optional_locals),
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        )
                                    });
                                    arg_vals.push(len.unwrap_or_else(|| b.const_i64(0)));
                                } else {
                                    arg_vals.push(b.const_i64(0));
                                }
                            } else {
                                arg_vals.push(b.const_i64(0));
                            }
                        }
                    }
                    arg_vals.extend(closure_args);
                    b.call(FuncRef::Indirect(target), arg_vals, IrType::Void);
                    finish_sequence_association_temps(b, &call_arg_sequence_temps);
                    deallocate_call_arg_array_temp_descriptors(b, &call_arg_array_temps);
                    deallocate_owned_string_bases(b, &call_arg_character_temps);
                }
            } else if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                // Elemental subroutine call with array actuals: F2018 §15.8.3
                // requires the call to be evaluated element-wise. Emit a loop
                // that drives one scalar call per element, with copy-in/copy-out
                // through per-iteration scalar temps for each array actual.
                if try_lower_elemental_subroutine_call(b, ctx, name, &key, args, callee.span) {
                    return;
                }
                // Intrinsic subroutines share the global procedure
                // namespace with user procedures. If USE association
                // or a local declaration made a callable with this
                // name visible, resolve that call normally instead
                // of eagerly lowering the intrinsic runtime hook.
                if user_callable_shadows_intrinsic(
                    ctx.st,
                    ctx.proc_scope_id,
                    b.func().name.as_str(),
                    &key,
                ) || !super::intrinsic_sub::lower_intrinsic_subroutine(b, ctx, &key, args)
                {
                    let procptr_target =
                        procedure_pointer_call_target(b, &ctx.locals, ctx.st, &key);
                    let signature_key = procptr_target
                        .as_ref()
                        .map(|(_, _, sig_key)| sig_key.clone())
                        .unwrap_or_else(|| key.clone());
                    // Not an intrinsic — general subroutine call.
                    // Keyword-argument reordering (F2003 §12.4.1.2).
                    // `call sub(b=10, a=20)` must bind by name, not
                    // position. reorder_args_by_keyword permutes the
                    // actual-arg list to match the callee's declared
                    // param order; the rest of the call-site code
                    // then runs positionally against that reordered
                    // list.
                    let resolution_arg_vals: Vec<ValueId> = args
                        .iter()
                        .map(|arg| match &arg.value {
                            crate::ast::expr::SectionSubscript::Element(e) => {
                                generic_dispatch_probe_value(
                                    b,
                                    &ctx.locals,
                                    e,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                    Some(ctx.internal_funcs),
                                    Some(ctx.contained_host_refs),
                                    Some(ctx.descriptor_params),
                                )
                            }
                            _ => b.const_i32(0),
                        })
                        .collect();
                    // Generic SUBROUTINE dispatch: if the callee name
                    // resolves to a NamedInterface symbol, replace it
                    // with the specific matched by the actual argument
                    // types. On failure, emit a diagnostic — the same
                    // rule as generic function-call resolution.
                    let (resolved_name, resolved_key) = if procptr_target.is_some() {
                        (name.clone(), signature_key.clone())
                    } else {
                        resolve_subroutine_call_name(
                            ctx.st,
                            b,
                            Some(&ctx.locals),
                            Some(ctx.type_layouts),
                            Some(ctx.internal_funcs),
                            name,
                            &key,
                            args,
                            &resolution_arg_vals,
                            callee.span,
                        )
                    };
                    let abi_lookup_keys = procedure_abi_lookup_keys_for_call_target(
                        ctx.st,
                        resolved_name.as_str(),
                        &[&resolved_key, &signature_key, &key],
                    );
                    let abi_primary_key = abi_lookup_keys
                        .first()
                        .map(String::as_str)
                        .unwrap_or(resolved_key.as_str());
                    let arg_order_key = if procptr_target.is_some() {
                        signature_key.as_str()
                    } else {
                        abi_lookup_keys
                            .iter()
                            .find(|k| {
                                callee_scope_for_lookup(ctx.st, k)
                                    .is_some_and(|scope| !scope.arg_order.is_empty())
                            })
                            .map(String::as_str)
                            .unwrap_or(resolved_key.as_str())
                    };
                    let arg_slots = reorder_args_by_keyword_slots(args, arg_order_key, ctx.st);
                    let value_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_value_arg_mask(ctx.st, k)
                    });
                    let desc_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        descriptor_param_mask_for_lookup(ctx.st, ctx.descriptor_params, k)
                    });
                    let bind_c_char_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_bind_c_char_arg_mask(ctx.st, k)
                    });
                    let char_len_star_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.char_len_star_params, k)
                            .or_else(|| callee_char_len_star_mask(ctx.st, k))
                    });
                    let pointer_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_pointer_arg_mask(ctx.st, k)
                    });
                    let allocatable_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_allocatable_arg_mask(ctx.st, k)
                    });
                    let class_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_class_arg_mask(ctx.st, k)
                    });
                    let string_desc_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_string_descriptor_arg_mask(ctx.st, k)
                    });
                    // If the callee has more parameters than provided args, and the
                    // trailing ones are OPTIONAL, pass null pointers so PRESENT() works.
                    let opt_flags = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.optional_params, k)
                            .or_else(|| callee_optional_arg_mask(ctx.st, k))
                    });
                    let sequence_array_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_sequence_array_arg_mask(ctx.st, k)
                    });
                    let sequence_array_copy_back_mask =
                        first_procedure_lookup(&abi_lookup_keys, |k| {
                            callee_sequence_array_copy_back_mask(ctx.st, k)
                        });
                    let mut arg_vals: Vec<ValueId> = Vec::with_capacity(arg_slots.len());
                    let mut arg_presence: Vec<ValueId> = Vec::with_capacity(arg_slots.len());
                    let mut call_arg_array_temps = Vec::new();
                    let mut call_arg_sequence_temps = Vec::new();
                    let mut call_arg_character_lens = vec![None; arg_slots.len()];
                    let mut call_arg_character_temps = Vec::new();
                    for (i, slot) in arg_slots.iter().enumerate() {
                        let is_value = value_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let mask_wants_descriptor = desc_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_bind_c_char = bind_c_char_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let expects_char_len_star = char_len_star_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_pointer = pointer_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let dummy_is_allocatable = allocatable_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let is_optional = opt_flags
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_string_descriptor = string_desc_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let dummy_is_class = mask_wants_descriptor
                            && class_mask
                                .as_ref()
                                .map(|mask| mask.get(i).copied().unwrap_or(false))
                                .unwrap_or(false);
                        let wants_string_descriptor = wants_string_descriptor && !wants_bind_c_char;
                        let wants_sequence_array = sequence_array_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let sequence_array_copy_back = sequence_array_copy_back_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let lowered = match slot {
                            Some(arg) => {
                                match &arg.value {
                                    crate::ast::expr::SectionSubscript::Element(arg_expr) => {
                                        let actual_is_array_section =
                                            actual_is_array_section_designator(
                                                &ctx.locals,
                                                arg_expr,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            );
                                        let actual_is_array = actual_expr_rank(
                                            arg_expr,
                                            &ctx.locals,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                        )
                                        .is_some_and(|rank| rank > 0)
                                            || actual_is_array_section;
                                        let actual_is_char_sequence =
                                            actual_is_character_array_section_designator(
                                                &ctx.locals,
                                                arg_expr,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            );
                                        let sequence_array_for_arg = (wants_sequence_array
                                            || actual_is_array)
                                            && !matches!(
                                                arg_expr.node,
                                                Expr::ConditionalExpr { .. } | Expr::NilArgument
                                            );
                                        let sequence_array_copy_back_for_arg =
                                            if wants_sequence_array {
                                                sequence_array_copy_back
                                            } else {
                                                true
                                            };
                                        // The materialization tree below is
                                        // reused per conditional-argument arm
                                        // (F2023): each arm builds its own
                                        // association in its own block.
                                        let mut materialize = |b: &mut FuncBuilder,
                                                               e: &crate::ast::expr::SpannedExpr|
                                         -> MaterializedCallArg {
                                    let mut character_len = None;
                                    let mut owned_character_bases = Vec::new();
                                    let wants_descriptor = (mask_wants_descriptor
                                        || (desc_mask.is_none()
                                            && actual_is_descriptor_backed(
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            )))
                                        && !wants_bind_c_char;
                                    let wants_polymorphic_descriptor = wants_descriptor
                                        && dummy_is_class
                                        && !dummy_is_allocatable
                                        && !wants_pointer;
                                    let value = if wants_bind_c_char {
                                        let actual = lower_bind_c_char_call_arg(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                            is_value,
                                        );
                                        character_len = actual.character_len;
                                        owned_character_bases = actual.owned_character_bases;
                                        actual.value
                                    } else if is_value {
                                        let raw = super::expr::lower_expr_full(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        );
                                        coerce_value_call_arg(b, ctx.st, abi_primary_key, i, raw)
                                    } else if wants_descriptor {
                                        let desc = lower_arg_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            wants_polymorphic_descriptor,
                                        );
                                        owned_character_bases
                                            .extend(b.take_owned_string_temp_bases(desc));
                                        desc
                                    } else if wants_string_descriptor {
                                        lower_arg_string_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                        )
                                    } else if wants_pointer {
                                        lower_pointer_dummy_actual(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        )
                                        .unwrap_or_else(|| {
                                            lower_arg_by_ref_for_dummy_full(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                dummy_is_class,
                                            )
                                        })
                                    } else if sequence_array_for_arg {
                                        let sequence_actual = if actual_is_char_sequence {
                                            lower_sequence_char_array_actual(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                sequence_array_copy_back_for_arg,
                                                &mut call_arg_sequence_temps,
                                            )
                                        } else {
                                            lower_sequence_array_actual(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                sequence_array_copy_back_for_arg,
                                                &mut call_arg_sequence_temps,
                                            )
                                        };
                                        sequence_actual
                                        .unwrap_or_else(|| {
                                            lower_arg_by_ref_for_dummy_full(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                dummy_is_class,
                                            )
                                        })
                                    } else {
                                        if let Some(actual) = lower_materialized_character_actual(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        ) {
                                            character_len = Some(actual.len);
                                            owned_character_bases = actual.owned_bases;
                                            actual.address
                                        } else {
                                            lower_arg_by_ref_for_dummy_full(
                                                b,
                                                &ctx.locals,
                                                e,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                                dummy_is_class,
                                            )
                                        }
                                    };
                                    let value = optional_arg_absent_if_forwarded_by_ref_dummy(
                                        b,
                                        &ctx.locals,
                                        e,
                                        is_optional && !is_value,
                                        value,
                                    );
                                    let value = optional_arg_absent_if_unallocated_allocatable(
                                        b,
                                        &ctx.locals,
                                        e,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                        is_optional
                                            && !is_value
                                            && !dummy_is_allocatable
                                            && !wants_bind_c_char
                                            && !wants_pointer,
                                        value,
                                    );
                                    if expects_char_len_star && character_len.is_none() {
                                        character_len = actual_char_arg_runtime_len(
                                            b,
                                            &ctx.locals,
                                            Some(&ctx.optional_locals),
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        );
                                    }
                                    MaterializedCallArg {
                                        value,
                                        character_len,
                                        owned_character_bases,
                                    }
                                    };
                                        let lowered = lower_call_arg_maybe_conditional(
                                            b,
                                            &ctx.locals,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                            arg_expr,
                                            abi_primary_key,
                                            i,
                                            is_value,
                                            &mut materialize,
                                        );
                                        call_arg_character_lens[i] = lowered.character_len;
                                        call_arg_character_temps
                                            .extend(lowered.owned_character_bases.iter().copied());
                                        let wants_descriptor = (mask_wants_descriptor
                                            || (desc_mask.is_none()
                                                && actual_is_descriptor_backed(
                                                    &ctx.locals,
                                                    arg_expr,
                                                    ctx.st,
                                                    Some(ctx.type_layouts),
                                                )))
                                            && !wants_bind_c_char;
                                        if wants_descriptor
                                            && !matches!(
                                                arg_expr.node,
                                                Expr::ConditionalExpr { .. } | Expr::NilArgument
                                            )
                                        {
                                            track_call_arg_array_temp_descriptor(
                                                b,
                                                &mut call_arg_array_temps,
                                                &ctx.locals,
                                                arg_expr,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                                lowered.value,
                                            );
                                        }
                                        lowered
                                    }
                                    _ => {
                                        let value = b.const_i32(0);
                                        let present = b.const_bool(true);
                                        LoweredCallArg::plain(value, present)
                                    }
                                }
                            }
                            None => {
                                let value = missing_optional_call_arg(
                                    b,
                                    ctx.st,
                                    abi_primary_key,
                                    i,
                                    is_value,
                                );
                                let present = b.const_bool(false);
                                LoweredCallArg::plain(value, present)
                            }
                        };
                        arg_vals.push(lowered.value);
                        arg_presence.push(lowered.present);
                    }
                    if let Some(opt_flags) = &opt_flags {
                        let missing_slots = arg_slots.len().saturating_sub(arg_vals.len());
                        for flag in opt_flags.iter().skip(arg_vals.len()).take(missing_slots) {
                            if *flag {
                                arg_vals.push(b.const_i64(0)); // null → absent
                            }
                        }
                    }
                    append_optional_value_presence_args(
                        b,
                        opt_flags.as_deref(),
                        value_mask.as_deref(),
                        &arg_presence,
                        &mut arg_vals,
                    );
                    // Hidden character-length ABI: for each callee
                    // param that is character(len=*), append the
                    // actual argument's string length as an i64.
                    if let Some(cls_flags) = &char_len_star_mask {
                        for (i, flag) in cls_flags.iter().enumerate() {
                            if !*flag || i >= arg_slots.len() {
                                continue;
                            }
                            if let Some(arg) = &arg_slots[i] {
                                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                                    let len = call_arg_character_lens[i].or_else(|| {
                                        actual_char_arg_runtime_len(
                                            b,
                                            &ctx.locals,
                                            Some(&ctx.optional_locals),
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        )
                                    });
                                    arg_vals.push(len.unwrap_or_else(|| b.const_i64(0)));
                                } else {
                                    arg_vals.push(b.const_i64(0));
                                }
                            } else {
                                arg_vals.push(b.const_i64(0));
                            }
                        }
                    }
                    if procptr_target.is_none() {
                        append_procedure_dummy_closure_args_for_call(
                            b,
                            &ctx.locals,
                            ctx.st,
                            &resolved_key,
                            &arg_slots,
                            Some(ctx.contained_host_refs),
                            &mut arg_vals,
                        );
                    } else if let Some((_, closure_args, _)) = &procptr_target {
                        arg_vals.extend(closure_args.iter().copied());
                    }
                    // Host-association closure-passing ABI: if the
                    // callee is a contained procedure, append one
                    // address per host-local variable it reads or
                    // writes. Caller must hold the matching variable
                    // in its own locals — this is guaranteed by the
                    // host-refs analysis that drove the callee
                    // signature, since both caller and callee share
                    // the same enclosing host.
                    if procptr_target.is_none() {
                        append_host_closure_args(b, ctx, &resolved_key, &mut arg_vals);
                    }
                    let func_ref = if let Some((target, _, _)) = procptr_target {
                        FuncRef::Indirect(target)
                    } else {
                        same_unit_func_ref(
                            ctx.st,
                            b.func().name.as_str(),
                            Some(ctx.internal_funcs),
                            &[&resolved_key],
                            resolved_name,
                        )
                    };
                    b.call(func_ref, arg_vals, IrType::Void);
                    finish_sequence_association_temps(b, &call_arg_sequence_temps);
                    deallocate_call_arg_array_temp_descriptors(b, &call_arg_array_temps);
                    deallocate_owned_string_bases(b, &call_arg_character_temps);
                }
            }
        }

        // ---- Control flow ----
        Stmt::IfConstruct {
            condition,
            then_body,
            else_ifs,
            else_body,
            ..
        } => {
            lower_if(b, ctx, condition, then_body, else_ifs, else_body);
        }

        Stmt::IfStmt { condition, action } => {
            let bb_then = b.create_block("if_then");
            let bb_end = b.create_block("if_end");
            lower_condition_branch(b, ctx, condition, bb_then, bb_end);

            b.set_block(bb_then);
            lower_stmt(b, ctx, action);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_end, vec![]);
            }

            b.set_block(bb_end);
        }

        Stmt::DoLoop {
            name,
            var,
            start,
            end,
            step,
            body,
            ..
        } => {
            lower_do_loop(
                b,
                ctx,
                DoLoopFields {
                    name,
                    var,
                    start,
                    end,
                    step,
                    body,
                    concurrent: false,
                },
            );
        }

        Stmt::DoConcurrent {
            name,
            controls,
            mask,
            body,
            ..
        } => {
            lower_do_concurrent(b, ctx, name, controls, mask.as_ref(), body, stmt.span);
        }

        Stmt::DoWhile {
            name,
            condition,
            body,
        } => {
            let bb_header = b.create_block("do_while_header");
            let bb_body = b.create_block("do_while_body");
            let bb_exit = b.create_block("do_while_exit");
            b.branch(bb_header, vec![]);

            ctx.push_loop(name.clone(), bb_header, bb_exit);

            b.set_block(bb_header);
            lower_condition_branch(b, ctx, condition, bb_body, bb_exit);

            b.set_block(bb_body);
            lower_stmts(b, ctx, body);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_header, vec![]);
            }

            ctx.pop_loop();
            b.set_block(bb_exit);
        }

        Stmt::SelectCase {
            selector, cases, ..
        } => {
            lower_select_case(b, ctx, selector, cases);
        }

        Stmt::WhereConstruct {
            mask,
            body,
            elsewhere,
            ..
        } => {
            let mut where_section_temps = Vec::new();
            let mut next_where_section_temp = 0usize;
            let prepared_mask = rewrite_where_read_sections_to_temps(
                b,
                ctx,
                mask,
                &mut next_where_section_temp,
                &mut where_section_temps,
            );
            let prepared_body: Vec<SpannedStmt> = body
                .iter()
                .map(|s| {
                    rewrite_where_read_sections_to_temps_stmt(
                        b,
                        ctx,
                        s,
                        &mut next_where_section_temp,
                        &mut where_section_temps,
                    )
                })
                .collect();
            let prepared_elsewhere: Vec<(Option<SpannedExpr>, Vec<SpannedStmt>)> = elsewhere
                .iter()
                .map(|(emask, ebody)| {
                    (
                        emask.clone(),
                        ebody
                            .iter()
                            .map(|s| {
                                rewrite_where_read_sections_to_temps_stmt(
                                    b,
                                    ctx,
                                    s,
                                    &mut next_where_section_temp,
                                    &mut where_section_temps,
                                )
                            })
                            .collect(),
                    )
                })
                .collect();

            // WHERE(mask) body [ELSEWHERE body] END WHERE
            // Collect ALL array names referenced in mask, body, OR
            // elsewhere body. Missing the elsewhere arm caused a
            // silent miscompile: an array reference appearing only
            // in elsewhere (e.g., `where (a > 0) c = a; elsewhere; c
            // = d`) was not scalarized, so `c = d` lowered through
            // the scalar path and silently produced 0.0 instead of
            // d(i).
            let mut array_names: Vec<String> = Vec::new();
            collect_array_names(&prepared_mask, &ctx.locals, &mut array_names);
            for s in &prepared_body {
                collect_array_names_stmt(s, &ctx.locals, &mut array_names);
            }
            for (emask, ebody) in &prepared_elsewhere {
                if let Some(emask) = emask {
                    collect_array_names(emask, &ctx.locals, &mut array_names);
                }
                for s in ebody {
                    collect_array_names_stmt(s, &ctx.locals, &mut array_names);
                }
            }

            if array_names.is_empty() {
                // No arrays — fall back to scalar IF-THEN-ELSE.
                let raw_cond = super::expr::lower_expr_ctx_tl(b, ctx, &prepared_mask);
                let cond = coerce_to_type(b, raw_cond, &IrType::Bool);
                let scalar_else_masks: Vec<Option<ValueId>> = prepared_elsewhere
                    .iter()
                    .map(|(emask, _)| {
                        emask.as_ref().map(|emask| {
                            let raw = super::expr::lower_expr_ctx_tl(b, ctx, emask);
                            coerce_to_type(b, raw, &IrType::Bool)
                        })
                    })
                    .collect();
                let bb_then = b.create_block("where_then");
                let bb_else = if prepared_elsewhere.is_empty() {
                    None
                } else {
                    Some(b.create_block("where_else_check"))
                };
                let bb_end = b.create_block("where_end");
                b.cond_branch(cond, bb_then, vec![], bb_else.unwrap_or(bb_end), vec![]);

                b.set_block(bb_then);
                lower_stmts(b, ctx, &prepared_body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(bb_end, vec![]);
                }

                if let Some(mut bb_check) = bb_else {
                    for (idx, ((_emask, else_body), scalar_mask)) in prepared_elsewhere
                        .iter()
                        .zip(scalar_else_masks.iter())
                        .enumerate()
                    {
                        b.set_block(bb_check);
                        if let Some(cond) = scalar_mask {
                            let bb_arm = b.create_block("where_else");
                            let bb_next = if idx + 1 < prepared_elsewhere.len() {
                                b.create_block("where_else_check")
                            } else {
                                bb_end
                            };
                            b.cond_branch(*cond, bb_arm, vec![], bb_next, vec![]);
                            b.set_block(bb_arm);
                            lower_stmts(b, ctx, else_body);
                            if b.func().block(b.current_block()).terminator.is_none() {
                                b.branch(bb_end, vec![]);
                            }
                            bb_check = bb_next;
                        } else {
                            lower_stmts(b, ctx, else_body);
                            if b.func().block(b.current_block()).terminator.is_none() {
                                b.branch(bb_end, vec![]);
                            }
                            break;
                        }
                    }
                }

                b.set_block(bb_end);
                finish_where_section_temps(b, ctx, &where_section_temps);
                return;
            }

            // Array-level WHERE: iterate over elements.
            // Use the first array to determine the iteration count. For
            // stack arrays `info.addr` is the raw element buffer — calling
            // afs_array_size on that would read garbage out of the rank
            // slot. array_total_elems_value picks the right source: it
            // materialises a descriptor query for descriptor-backed locals
            // and folds dims to a constant for explicit-shape stack arrays.
            let first_arr_name = &array_names[0];
            let first_arr = ctx
                .locals
                .get(first_arr_name)
                .cloned()
                .expect("array must exist");
            let n = array_total_elems_value(b, &first_arr);

            // Get base addresses for all arrays (loaded once outside the loop).
            let mut array_bases: HashMap<String, ValueId> = HashMap::new();
            for arr_name in &array_names {
                if let Some(info) = ctx.locals.get(arr_name) {
                    let base = array_base_addr(b, info);
                    array_bases.insert(arr_name.clone(), base);
                }
            }

            if prepared_elsewhere.is_empty() {
                lower_where_array_pass(
                    b,
                    ctx,
                    &array_names,
                    &array_bases,
                    n,
                    &prepared_body,
                    |b, ctx, _i_val| {
                        let rewritten_mask =
                            rewrite_scalarized_section_refs(&prepared_mask, &array_names);
                        super::expr::lower_expr_ctx_tl(b, ctx, &rewritten_mask)
                    },
                );
                finish_where_section_temps(b, ctx, &where_section_temps);
                return;
            }

            if prepared_elsewhere.len() == 1 && prepared_elsewhere[0].0.is_none() {
                lower_where_array_if_else_pass(
                    b,
                    ctx,
                    &array_names,
                    &array_bases,
                    n,
                    &prepared_body,
                    &prepared_elsewhere[0].1,
                    |b, ctx, _i_val| {
                        let rewritten_mask =
                            rewrite_scalarized_section_refs(&prepared_mask, &array_names);
                        super::expr::lower_expr_ctx_tl(b, ctx, &rewritten_mask)
                    },
                );
                finish_where_section_temps(b, ctx, &where_section_temps);
                return;
            }

            let main_mask_value = lower_where_mask_value(b, ctx, &prepared_mask);
            lower_where_array_pass(
                b,
                ctx,
                &array_names,
                &array_bases,
                n,
                &prepared_body,
                |b, _ctx, i_val| where_mask_value_at(b, &main_mask_value, i_val),
            );

            let mut elsewhere_mask_values: Vec<WhereMaskValue> = Vec::new();
            for (else_mask_expr, else_body) in &prepared_elsewhere {
                if let Some(mask_expr) = else_mask_expr {
                    let prepared_else_mask = rewrite_where_read_sections_to_temps(
                        b,
                        ctx,
                        mask_expr,
                        &mut next_where_section_temp,
                        &mut where_section_temps,
                    );
                    let else_mask = lower_where_mask_value(b, ctx, &prepared_else_mask);
                    lower_where_array_pass(
                        b,
                        ctx,
                        &array_names,
                        &array_bases,
                        n,
                        else_body,
                        |b, _ctx, i_val| {
                            let pending = where_pending_mask_at(
                                b,
                                i_val,
                                &main_mask_value,
                                &elsewhere_mask_values,
                            );
                            let masked = where_mask_value_at(b, &else_mask, i_val);
                            b.and(pending, masked)
                        },
                    );
                    elsewhere_mask_values.push(else_mask);
                } else {
                    lower_where_array_pass(
                        b,
                        ctx,
                        &array_names,
                        &array_bases,
                        n,
                        else_body,
                        |b, _ctx, i_val| {
                            where_pending_mask_at(
                                b,
                                i_val,
                                &main_mask_value,
                                &elsewhere_mask_values,
                            )
                        },
                    );
                    break;
                }
            }

            let mut mask_values: Vec<&WhereMaskValue> = Vec::new();
            mask_values.push(&main_mask_value);
            for mask in &elsewhere_mask_values {
                mask_values.push(mask);
            }
            finish_where_mask_values(b, &mask_values);
            finish_where_section_temps(b, ctx, &where_section_temps);
        }

        Stmt::WhereStmt { mask, stmt } => {
            let mut where_section_temps = Vec::new();
            let mut next_where_section_temp = 0usize;
            let prepared_mask = rewrite_where_read_sections_to_temps(
                b,
                ctx,
                mask,
                &mut next_where_section_temp,
                &mut where_section_temps,
            );
            let prepared_stmt = rewrite_where_read_sections_to_temps_stmt(
                b,
                ctx,
                stmt,
                &mut next_where_section_temp,
                &mut where_section_temps,
            );

            // Single-line WHERE: where (cond) assignment.
            // F2018 §10.2.3.2: when the mask is an array-valued logical
            // expression, the assignment runs element-wise under the
            // mask. Reuse the WhereConstruct array-iteration shape: set
            // up per-element bindings for every array referenced in the
            // mask or assignment, evaluate the scalar mask, and run the
            // assignment under it.
            let mut array_names: Vec<String> = Vec::new();
            collect_array_names(&prepared_mask, &ctx.locals, &mut array_names);
            collect_array_names_stmt(&prepared_stmt, &ctx.locals, &mut array_names);

            if array_names.is_empty() {
                let raw_cond = super::expr::lower_expr_ctx_tl(b, ctx, &prepared_mask);
                let cond = coerce_to_type(b, raw_cond, &IrType::Bool);
                let bb_then = b.create_block("where_stmt");
                let bb_end = b.create_block("where_stmt_end");
                b.cond_branch(cond, bb_then, vec![], bb_end, vec![]);
                b.set_block(bb_then);
                lower_stmt(b, ctx, &prepared_stmt);
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(bb_end, vec![]);
                }
                b.set_block(bb_end);
                finish_where_section_temps(b, ctx, &where_section_temps);
                return;
            }

            let first_arr_name = &array_names[0];
            let first_arr = ctx
                .locals
                .get(first_arr_name)
                .cloned()
                .expect("array must exist");
            let n = array_total_elems_value(b, &first_arr);

            let mut array_bases: HashMap<String, ValueId> = HashMap::new();
            for arr_name in &array_names {
                if let Some(info) = ctx.locals.get(arr_name) {
                    let base = array_base_addr(b, info);
                    array_bases.insert(arr_name.clone(), base);
                }
            }

            let i_addr = b.alloca(IrType::Int(IntWidth::I64));
            let i_zero = b.const_i64(0);
            b.store(i_zero, i_addr);

            let bb_check = b.create_block("where_stmt_check");
            let bb_body = b.create_block("where_stmt_body");
            let bb_exit = b.create_block("where_stmt_exit");
            b.branch(bb_check, vec![]);

            b.set_block(bb_check);
            let i = b.load(i_addr);
            let done = b.icmp(CmpOp::Ge, i, n);
            b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

            b.set_block(bb_body);
            let i_val = b.load(i_addr);

            let mut saved_locals: Vec<(String, Option<LocalInfo>)> = Vec::new();
            for arr_name in &array_names {
                saved_locals.push((arr_name.clone(), ctx.locals.get(arr_name).cloned()));
                if let Some(orig_info) = ctx.locals.get(arr_name).cloned() {
                    let base = *array_bases.get(arr_name).unwrap();
                    let elem_bytes_val = array_elem_size_value(b, &orig_info);
                    let byte_off = b.imul(i_val, elem_bytes_val);
                    let elem_ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
                    ctx.locals.insert(
                        arr_name.clone(),
                        LocalInfo {
                            addr: elem_ptr,
                            ty: orig_info.ty.clone(),
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
            }

            // See WhereConstruct: residual `name(section)` calls in the
            // mask/stmt would emit undefined externals after the
            // substitution. Fold them to bare `Name` so the per-iter
            // scalar binding picks up.
            let rewritten_mask = rewrite_scalarized_section_refs(&prepared_mask, &array_names);
            let rewritten_stmt = rewrite_scalarized_section_refs_stmt(&prepared_stmt, &array_names);

            let cond_raw = super::expr::lower_expr_ctx_tl(b, ctx, &rewritten_mask);
            let cond = coerce_to_type(b, cond_raw, &IrType::Bool);
            let bb_then = b.create_block("where_stmt_then");
            let bb_incr = b.create_block("where_stmt_incr");
            b.cond_branch(cond, bb_then, vec![], bb_incr, vec![]);

            b.set_block(bb_then);
            lower_stmt(b, ctx, &rewritten_stmt);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_incr, vec![]);
            }

            b.set_block(bb_incr);
            for (name, orig) in saved_locals {
                if let Some(info) = orig {
                    ctx.locals.insert(name, info);
                } else {
                    ctx.locals.remove(&name);
                }
            }
            let i_cur = b.load(i_addr);
            let one = b.const_i64(1);
            let next = b.iadd(i_cur, one);
            b.store(next, i_addr);
            b.branch(bb_check, vec![]);

            b.set_block(bb_exit);
            finish_where_section_temps(b, ctx, &where_section_temps);
        }

        Stmt::ForallConstruct {
            specs, mask, body, ..
        } => {
            if try_lower_forall_assignment_with_temp(b, ctx, specs, mask.as_ref(), body) {
                return;
            }
            // FORALL: nest loops. The body goes inside the innermost loop.
            // Build the body statements including optional mask as a closure-like pattern.
            // The innermost loop gets the real body; outer loops wrap it.
            lower_forall_nested(b, ctx, specs, mask.as_ref(), body);
        }

        Stmt::ForallStmt { specs, mask, stmt } => {
            let body_vec = vec![(**stmt).clone()];
            if try_lower_forall_assignment_with_temp(b, ctx, specs, mask.as_ref(), &body_vec) {
                return;
            }
            lower_forall_nested(b, ctx, specs, mask.as_ref(), &body_vec);
        }

        Stmt::SelectType {
            selector,
            guards,
            assoc_name,
            ..
        } => {
            let bb_end = b.create_block("select_type_end");
            let selector_type = operator_expr_type_info(
                selector,
                Some(&ctx.locals),
                ctx.st,
                Some(ctx.type_layouts),
            );
            let selector_info = associate_alias_local_info(b, ctx, selector);
            let selector_is_unlimited = matches!(
                selector_type.as_ref(),
                Some(crate::sema::symtab::TypeInfo::ClassStar)
            );
            let dynamic_class_selector = selector_info.as_ref().filter(|info| {
                local_uses_array_descriptor(info)
                    && (info.derived_type.is_some() || info.is_class || selector_is_unlimited)
            });

            if let Some(info) = dynamic_class_selector {
                let desc = array_descriptor_addr(b, info);
                let tag_val = load_array_desc_type_tag(b, desc);
                let default_body = guards.iter().find_map(|guard| match guard {
                    crate::ast::stmt::TypeGuard::ClassDefault { body } => Some(body),
                    _ => None,
                });
                for guard in guards {
                    match guard {
                        crate::ast::stmt::TypeGuard::TypeIs {
                            type_name: guard_type,
                            body,
                        } => {
                            if let Some(guard_tag) =
                                intrinsic_class_star_type_tag_for_guard(guard_type, Some(ctx.st))
                            {
                                let guard_tag = b.const_i64(guard_tag as i64);
                                let matches = b.icmp(CmpOp::Eq, tag_val, guard_tag);
                                let bb_match = b.create_block("type_is_intrinsic_match");
                                let bb_next = b.create_block("type_is_intrinsic_next");
                                b.cond_branch(matches, bb_match, vec![], bb_next, vec![]);

                                b.set_block(bb_match);
                                with_select_type_intrinsic_guard_binding(
                                    b,
                                    ctx,
                                    selector,
                                    assoc_name.as_deref(),
                                    guard_type,
                                    |b, ctx| lower_stmts(b, ctx, body),
                                );
                                if b.func().block(b.current_block()).terminator.is_none() {
                                    b.branch(bb_end, vec![]);
                                }

                                b.set_block(bb_next);
                            } else if let Some(guard_layout) = ctx.type_layouts.get(guard_type) {
                                let guard_tag = b.const_i64(guard_layout.type_tag as i64);
                                let matches = b.icmp(CmpOp::Eq, tag_val, guard_tag);
                                let bb_match = b.create_block("type_is_match");
                                let bb_next = b.create_block("type_is_next");
                                b.cond_branch(matches, bb_match, vec![], bb_next, vec![]);

                                b.set_block(bb_match);
                                with_select_type_guard_binding(
                                    b,
                                    ctx,
                                    selector,
                                    assoc_name.as_deref(),
                                    guard_type,
                                    |b, ctx| lower_stmts(b, ctx, body),
                                );
                                if b.func().block(b.current_block()).terminator.is_none() {
                                    b.branch(bb_end, vec![]);
                                }

                                b.set_block(bb_next);
                            }
                        }
                        crate::ast::stmt::TypeGuard::ClassIs {
                            type_name: guard_type,
                            body,
                        } => {
                            let mut matching_tags: Vec<u64> = ctx
                                .type_layouts
                                .layouts
                                .values()
                                .filter(|layout| !layout.is_abstract)
                                .filter(|layout| {
                                    is_type_or_extends(&layout.name, guard_type, ctx.type_layouts)
                                })
                                .map(|layout| layout.type_tag)
                                .collect();
                            matching_tags.sort_unstable();
                            if !matching_tags.is_empty() {
                                let mut matches = None;
                                for tag in matching_tags {
                                    let tag_val_const = b.const_i64(tag as i64);
                                    let eq = b.icmp(CmpOp::Eq, tag_val, tag_val_const);
                                    matches = Some(match matches {
                                        Some(prev) => b.or(prev, eq),
                                        None => eq,
                                    });
                                }
                                let bb_match = b.create_block("class_is_match");
                                let bb_next = b.create_block("class_is_next");
                                b.cond_branch(
                                    matches.expect("non-empty CLASS IS candidate set"),
                                    bb_match,
                                    vec![],
                                    bb_next,
                                    vec![],
                                );

                                b.set_block(bb_match);
                                with_select_type_guard_binding(
                                    b,
                                    ctx,
                                    selector,
                                    assoc_name.as_deref(),
                                    guard_type,
                                    |b, ctx| lower_stmts(b, ctx, body),
                                );
                                if b.func().block(b.current_block()).terminator.is_none() {
                                    b.branch(bb_end, vec![]);
                                }

                                b.set_block(bb_next);
                            }
                        }
                        crate::ast::stmt::TypeGuard::ClassDefault { .. } => {}
                    }
                }
                if let Some(body) = default_body {
                    lower_stmts(b, ctx, body);
                    if b.func().block(b.current_block()).terminator.is_none() {
                        b.branch(bb_end, vec![]);
                    }
                }
            } else {
                if matches!(selector_type, Some(crate::sema::symtab::TypeInfo::Class(_))) {
                    eprintln!(
                        "armfortas: error: {}:{}: SELECT TYPE on polymorphic CLASS(...) selectors is not implemented yet",
                        stmt.span.start.line, stmt.span.start.col
                    );
                    let _ = std::io::stderr().flush();
                    std::process::exit(1);
                }
                let static_type = selector_info
                    .as_ref()
                    .and_then(|info| info.derived_type.clone());
                if let Some(ref type_name) = static_type {
                    if let Some(layout) = ctx.type_layouts.get(type_name) {
                        let tag_val = b.const_i64(layout.type_tag as i64);
                        let default_body = guards.iter().find_map(|guard| match guard {
                            crate::ast::stmt::TypeGuard::ClassDefault { body } => Some(body),
                            _ => None,
                        });

                        for guard in guards {
                            match guard {
                                crate::ast::stmt::TypeGuard::TypeIs {
                                    type_name: guard_type,
                                    body,
                                } => {
                                    if let Some(guard_layout) = ctx.type_layouts.get(guard_type) {
                                        let guard_tag = b.const_i64(guard_layout.type_tag as i64);
                                        let matches = b.icmp(CmpOp::Eq, tag_val, guard_tag);
                                        let bb_match = b.create_block("type_is_match");
                                        let bb_next = b.create_block("type_is_next");
                                        b.cond_branch(matches, bb_match, vec![], bb_next, vec![]);

                                        b.set_block(bb_match);
                                        with_select_type_guard_binding(
                                            b,
                                            ctx,
                                            selector,
                                            assoc_name.as_deref(),
                                            guard_type,
                                            |b, ctx| lower_stmts(b, ctx, body),
                                        );
                                        if b.func().block(b.current_block()).terminator.is_none() {
                                            b.branch(bb_end, vec![]);
                                        }

                                        b.set_block(bb_next);
                                    } else {
                                        // Unknown guard type — skip.
                                        let tag_matches =
                                            type_name.eq_ignore_ascii_case(guard_type);
                                        if tag_matches {
                                            with_select_type_guard_binding(
                                                b,
                                                ctx,
                                                selector,
                                                assoc_name.as_deref(),
                                                guard_type,
                                                |b, ctx| lower_stmts(b, ctx, body),
                                            );
                                            if b.func()
                                                .block(b.current_block())
                                                .terminator
                                                .is_none()
                                            {
                                                b.branch(bb_end, vec![]);
                                            }
                                            break;
                                        }
                                    }
                                }
                                crate::ast::stmt::TypeGuard::ClassIs {
                                    type_name: guard_type,
                                    body,
                                } => {
                                    // CLASS IS matches the type or any extension.
                                    // Check if static type is or extends the guard type.
                                    let is_match =
                                        is_type_or_extends(type_name, guard_type, ctx.type_layouts);
                                    if is_match {
                                        with_select_type_guard_binding(
                                            b,
                                            ctx,
                                            selector,
                                            assoc_name.as_deref(),
                                            guard_type,
                                            |b, ctx| lower_stmts(b, ctx, body),
                                        );
                                        if b.func().block(b.current_block()).terminator.is_none() {
                                            b.branch(bb_end, vec![]);
                                        }
                                        break; // CLASS IS matched, skip remaining guards.
                                    }
                                }
                                crate::ast::stmt::TypeGuard::ClassDefault { .. } => {}
                            }
                        }
                        if let Some(body) = default_body {
                            lower_stmts(b, ctx, body);
                            if b.func().block(b.current_block()).terminator.is_none() {
                                b.branch(bb_end, vec![]);
                            }
                        }
                    }
                }
            }

            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_end, vec![]);
            }
            b.set_block(bb_end);
        }

        Stmt::Exit { name } => {
            if let Some(lp) = ctx.find_loop(name) {
                let exit = lp.exit;
                b.branch(exit, vec![]);
            } else if let Some(exit) = ctx.find_construct_exit(name) {
                b.branch(exit, vec![]);
            }
        }

        Stmt::Cycle { name } => {
            if let Some(lp) = ctx.find_loop(name) {
                let header = lp.header;
                b.branch(header, vec![]);
            }
        }

        Stmt::Return { .. } => {
            if ctx.hidden_result_abi == HiddenResultAbi::StringDescriptor {
                lower_hidden_string_result_copy(b, ctx);
            }
            let skip = if matches!(
                ctx.hidden_result_abi,
                HiddenResultAbi::ArrayDescriptor | HiddenResultAbi::DerivedAggregate
            ) {
                Some(ValueId(0))
            } else {
                None
            };
            insert_implicit_dealloc(
                b,
                &ctx.locals,
                &ctx.locals,
                ctx.type_layouts,
                ctx.st,
                ctx.internal_funcs,
                Some(ctx.contained_host_refs),
                skip,
                true,
            );
            if ctx.hidden_result_abi != HiddenResultAbi::None {
                // sret convention: result was written into the hidden first param.
                b.ret(None);
            } else if let Some(addr) = ctx.result_addr {
                let returns_derived_buffer = ctx
                    .result_name
                    .as_ref()
                    .and_then(|name| ctx.locals.get(name))
                    .map(|info| !info.is_pointer && info.derived_type.is_some())
                    .unwrap_or(false);
                if returns_derived_buffer {
                    // Derived-type function results use the pointer-return
                    // convention; explicit RETURN must mirror the implicit
                    // fallthrough path instead of loading the aggregate bytes.
                    let zero = b.const_i64(0);
                    let byte_ptr = b.gep(addr, vec![zero], IrType::Int(IntWidth::I8));
                    b.ret(Some(byte_ptr));
                } else {
                    let rv = b.load(addr);
                    b.ret(Some(rv));
                }
            } else {
                b.ret_void();
            }
        }

        Stmt::Stop { code, .. } => {
            enum StopCode {
                Msg(ValueId, ValueId),
                Int(ValueId),
                None,
            }
            let stop_code = if let Some(code_expr) = code {
                let is_char = expr_is_character_expr(
                    b,
                    &ctx.locals,
                    code_expr,
                    ctx.st,
                    Some(ctx.type_layouts),
                ) || matches!(code_expr.node, Expr::StringLiteral { .. });
                if is_char {
                    let (ptr, len) = lower_string_expr_ctx(b, ctx, code_expr);
                    StopCode::Msg(ptr, len)
                } else {
                    let val = super::expr::lower_expr_ctx(b, ctx, code_expr);
                    let val_ty = b
                        .func()
                        .value_type(val)
                        .unwrap_or(IrType::Int(IntWidth::I64));
                    StopCode::Int(match val_ty {
                        IrType::Int(IntWidth::I64) => val,
                        IrType::Int(_) => b.int_extend(val, IntWidth::I64, true),
                        _ => val,
                    })
                }
            } else {
                StopCode::None
            };
            if !matches!(stop_code, StopCode::Msg(..)) {
                let skip = if matches!(
                    ctx.hidden_result_abi,
                    HiddenResultAbi::ArrayDescriptor | HiddenResultAbi::DerivedAggregate
                ) {
                    Some(ValueId(0))
                } else {
                    None
                };
                insert_implicit_dealloc(
                    b,
                    &ctx.locals,
                    &ctx.locals,
                    ctx.type_layouts,
                    ctx.st,
                    ctx.internal_funcs,
                    Some(ctx.contained_host_refs),
                    skip,
                    true,
                );
            }
            match stop_code {
                StopCode::Msg(ptr, len) => {
                    b.call(
                        FuncRef::External("afs_stop_msg".into()),
                        vec![ptr, len],
                        IrType::Void,
                    );
                }
                StopCode::Int(widened) => {
                    b.call(
                        FuncRef::External("afs_stop_int".into()),
                        vec![widened],
                        IrType::Void,
                    );
                }
                StopCode::None => {
                    b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
                }
            }
            b.unreachable();
        }
        Stmt::ErrorStop { code, .. } => {
            // F2018 §11.4: error stop with a stop-code prints the
            // implementation banner together with the user's code. The
            // earlier lowering threw the code away so all stdlib error
            // diagnostics surfaced as bare "ERROR STOP" — masking real
            // problems such as stdlib_sorting's "work array is too small"
            // and "Allocation of adjoint_array buffer failed". Dispatch
            // to the message or integer entry depending on stop-code type.
            //
            // Lower the stop-code expression BEFORE the implicit dealloc:
            // for an allocatable character stop-code (e.g. stdlib's
            // `error stop err_msg` where err_msg is character(:),
            // allocatable), the dealloc nullifies the descriptor's data
            // pointer, so loading after dealloc gives a null ptr and
            // afs_error_stop_msg falls back to the bare "ERROR STOP"
            // branch. Capturing first preserves the pointer for the
            // call (the buffer remains mapped through process exit).
            enum StopCode {
                Msg(ValueId, ValueId),
                Int(ValueId),
                None,
            }
            let stop_code = if let Some(code_expr) = code {
                let is_char = expr_is_character_expr(
                    b,
                    &ctx.locals,
                    code_expr,
                    ctx.st,
                    Some(ctx.type_layouts),
                ) || matches!(code_expr.node, Expr::StringLiteral { .. });
                if is_char {
                    let (ptr, len) = lower_string_expr_ctx(b, ctx, code_expr);
                    StopCode::Msg(ptr, len)
                } else {
                    let val = super::expr::lower_expr_ctx(b, ctx, code_expr);
                    let val_ty = b
                        .func()
                        .value_type(val)
                        .unwrap_or(IrType::Int(IntWidth::I64));
                    let widened = match val_ty {
                        IrType::Int(IntWidth::I64) => val,
                        IrType::Int(_) => b.int_extend(val, IntWidth::I64, true),
                        _ => val,
                    };
                    StopCode::Int(widened)
                }
            } else {
                StopCode::None
            };

            // Skip implicit dealloc for character-stop-code error stops.
            // The user-provided message often references a local
            // allocatable string whose buffer would be freed by the
            // dealloc, leaving afs_error_stop_msg reading freed memory
            // (or, if the load order let it run before dealloc, a now-
            // null data pointer). Process exit cleans up the heap
            // anyway. For integer / no-code stops the dealloc still
            // runs to satisfy any non-error cleanup expectations.
            if !matches!(stop_code, StopCode::Msg(..)) {
                let skip = if matches!(
                    ctx.hidden_result_abi,
                    HiddenResultAbi::ArrayDescriptor | HiddenResultAbi::DerivedAggregate
                ) {
                    Some(ValueId(0))
                } else {
                    None
                };
                insert_implicit_dealloc(
                    b,
                    &ctx.locals,
                    &ctx.locals,
                    ctx.type_layouts,
                    ctx.st,
                    ctx.internal_funcs,
                    Some(ctx.contained_host_refs),
                    skip,
                    true,
                );
            }

            match stop_code {
                StopCode::Msg(ptr, len) => {
                    b.call(
                        FuncRef::External("afs_error_stop_msg".into()),
                        vec![ptr, len],
                        IrType::Void,
                    );
                }
                StopCode::Int(widened) => {
                    b.call(
                        FuncRef::External("afs_error_stop_int".into()),
                        vec![widened],
                        IrType::Void,
                    );
                }
                StopCode::None => {
                    b.runtime_call(RuntimeFunc::ErrorStop, vec![], IrType::Void);
                }
            }
            b.unreachable();
        }

        Stmt::Allocate {
            type_spec,
            items,
            opts,
        } => {
            let stat_target = super::core::allocate_status_target(b, ctx, opts);
            let stat_addr = stat_target.runtime_addr;
            let runtime_stat_arg = if allocate_keyword_expr(opts, "stat").is_some() {
                stat_addr
            } else {
                b.const_i64(0)
            };
            // F2018 §9.7.1.3: stat-variable is 0 on success. Pre-zero so
            // any item path that doesn't update stat_addr (e.g. scalar
            // simple allocates that don't go through a runtime helper)
            // leaves the user's stat at SUCCESS rather than the
            // uninitialized garbage that previously surfaced as the
            // stdlib_bitsets "allocation fault for STRING" miscall.
            // Failing item paths still overwrite stat_addr through their
            // runtime helpers.
            {
                let zero_i32 = b.const_i32(0);
                b.store(zero_i32, stat_addr);
            }
            let errmsg_target = allocate_errmsg_target(b, ctx, opts);
            let typed_char_len = typed_allocate_char_len(
                b,
                &ctx.locals,
                type_spec.as_ref(),
                ctx.st,
                Some(ctx.type_layouts),
            );
            let typed_type_tag =
                typed_allocate_type_tag_value(b, type_spec.as_ref(), ctx.type_layouts);
            let typed_vtable = typed_allocate_vtable_value(b, type_spec.as_ref(), ctx.type_layouts);
            let typed_layout = typed_allocate_layout(type_spec.as_ref(), ctx.type_layouts);
            let source_expr = allocate_keyword_expr(opts, "source");
            let source_scalar_desc = allocate_scalar_source_descriptor(b, ctx, opts);
            let source_desc = allocate_descriptor_keyword_expr(b, ctx, opts, "source");
            let mold_desc = allocate_descriptor_keyword_expr(b, ctx, opts, "mold");
            let mold_expr = allocate_keyword_expr(opts, "mold");
            let mold_static_layout = static_concrete_expr_type_layout(ctx, mold_expr);
            let mold_static_type_tag =
                mold_static_layout.map(|layout| b.const_i64(layout.type_tag as i64));
            let mold_static_vtable =
                mold_static_layout.and_then(|layout| type_layout_vtable_value(b, layout));
            let mold_rank = mold_expr
                .and_then(|expr| {
                    actual_expr_rank(expr, &ctx.locals, ctx.st, Some(ctx.type_layouts))
                })
                .unwrap_or(0);
            let mold_shape_desc = if mold_static_layout.is_some() && mold_rank == 0 {
                None
            } else {
                mold_desc
            };
            let shape_desc = source_desc.or(mold_shape_desc).or(source_scalar_desc);
            let allocate_done_bb = b.create_block("allocate_done");

            for (item_idx, item) in items.iter().enumerate() {
                if item_idx > 0 {
                    let item_stat = b.load_typed(stat_addr, IrType::Int(IntWidth::I32));
                    let zero_i32 = b.const_i32(0);
                    let item_ok = b.icmp(CmpOp::Eq, item_stat, zero_i32);
                    let allocate_item_bb = b.create_block("allocate_item");
                    b.cond_branch(item_ok, allocate_item_bb, vec![], allocate_done_bb, vec![]);
                    b.set_block(allocate_item_bb);
                }
                let source_char = allocate_char_source_value(b, ctx, opts);
                let char_alloc_len = typed_char_len
                    .or_else(|| source_char.as_ref().map(|(_, len)| *len))
                    .or_else(|| allocate_char_mold_len(b, ctx, opts));
                let component_alloc = match &item.node {
                    Expr::ComponentAccess { .. } => Some((item, &[][..])),
                    Expr::FunctionCall { callee, args }
                        if matches!(callee.node, Expr::ComponentAccess { .. }) =>
                    {
                        Some((callee.as_ref(), args.as_slice()))
                    }
                    _ => None,
                };
                if let Some((component_expr, args)) = component_alloc {
                    if let Some((field_ptr, field_owner_layout, field)) =
                        resolve_component_field_access_with_owner(
                            b,
                            &ctx.locals,
                            component_expr,
                            ctx.st,
                            ctx.type_layouts,
                        )
                    {
                        if matches!(field_char_kind(&field), CharKind::Deferred) && field.size == 32
                        {
                            let Some(len_val) = char_alloc_len else {
                                eprintln!(
                                        "armfortas: error: {}:{}: deferred-length character ALLOCATE requires a typed length or SOURCE/MOLD support",
                                        stmt.span.start.line, stmt.span.start.col
                                    );
                                let _ = std::io::stderr().flush();
                                std::process::exit(1);
                            };
                            b.call(
                                FuncRef::External("afs_allocate_string".into()),
                                vec![field_ptr, len_val, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "ALLOCATE failed",
                            );
                            if let Some((src_ptr, src_len)) = source_char {
                                let alloc_stat =
                                    b.load_typed(stat_addr, IrType::Int(IntWidth::I32));
                                let zero_i32 = b.const_i32(0);
                                let alloc_ok = b.icmp(CmpOp::Eq, alloc_stat, zero_i32);
                                let copy_bb = b.create_block("allocate_char_source_copy");
                                let done_bb = b.create_block("allocate_char_source_done");
                                b.cond_branch(alloc_ok, copy_bb, vec![], done_bb, vec![]);
                                b.set_block(copy_bb);
                                b.call(
                                    FuncRef::External("afs_assign_char_deferred".into()),
                                    vec![field_ptr, src_ptr, src_len],
                                    IrType::Void,
                                );
                                b.branch(done_bb, vec![]);
                                b.set_block(done_bb);
                            }
                            continue;
                        }
                        if field.size == 392 && (field.allocatable || field.pointer) {
                            let elem_ty = field_storage_ir_type(&field, ctx.type_layouts);
                            let field_is_class_star = matches!(
                                &field.type_info,
                                crate::sema::symtab::TypeInfo::ClassStar
                            );
                            let bounds = lower_alloc_bounds_list(b, ctx, args);
                            let rank = bounds.len();
                            let field_info = LocalInfo {
                                addr: field_ptr,
                                ty: elem_ty.clone(),
                                dims: vec![],
                                allocatable: true,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: field_char_kind(&field),
                                derived_type: field_derived_type_name(&field),
                                inline_const: None,
                                is_pointer: field.pointer,
                                runtime_dim_upper: vec![],
                                is_class: matches!(
                                    &field.type_info,
                                    crate::sema::symtab::TypeInfo::Class(_)
                                        | crate::sema::symtab::TypeInfo::ClassStar
                                ),
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            };
                            let source_scalar_layout = if source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_type_layout(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let source_scalar_type = if source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_derived_type_name(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let source_intrinsic_elem_ty = if field_is_class_star
                                && source_desc.is_none()
                            {
                                source_expr
                                    .and_then(|expr| class_star_intrinsic_source_ir_type(ctx, expr))
                            } else {
                                None
                            };
                            let source_char_elem_size =
                                if field_is_class_star && rank == 0 && source_char.is_some() {
                                    char_alloc_len
                                } else {
                                    None
                                };
                            let dynamic_layout = source_scalar_layout
                                .or(mold_static_layout)
                                .or(typed_layout)
                                .or_else(|| {
                                    field_info.derived_type.as_deref().and_then(|type_name| {
                                        ctx.type_layouts.get_related(field_owner_layout, type_name)
                                    })
                                });
                            let target_rank =
                                local_declared_rank(&field_info).max(if field.declared_array {
                                    field.dims.len().max(1)
                                } else {
                                    0
                                });
                            let source_copy_rank = rank.max(target_rank);
                            let source_copy_plan = source_expr.and_then(|expr| {
                                expr_scalar_alloc_source_copy_plan(
                                    expr,
                                    &ctx.locals,
                                    ctx.st,
                                    ctx.type_layouts,
                                )
                            });
                            let array_source_copy_layout = if source_desc.is_some() {
                                if source_copy_rank == 0 && source_copy_plan.is_some() {
                                    None
                                } else {
                                    match source_copy_plan.as_ref() {
                                        Some(ScalarAllocSourceCopyPlan::Static(type_name)) => {
                                            ctx.type_layouts.get(type_name).filter(|layout| {
                                                derived_layout_needs_deep_copy(
                                                    layout,
                                                    ctx.type_layouts,
                                                )
                                            })
                                        }
                                        Some(
                                            ScalarAllocSourceCopyPlan::Dynamic(_)
                                            | ScalarAllocSourceCopyPlan::UnlimitedPolymorphic,
                                        ) => None,
                                        None => dynamic_layout.filter(|layout| {
                                            derived_layout_needs_deep_copy(layout, ctx.type_layouts)
                                        }),
                                    }
                                }
                            } else {
                                None
                            };
                            let elem_size_bytes = dynamic_layout
                                .map(|layout| layout.size as i64)
                                .or_else(|| {
                                    source_intrinsic_elem_ty
                                        .as_ref()
                                        .map(|ty| ir_scalar_byte_size(ty, ctx.layout))
                                })
                                .unwrap_or_else(|| {
                                    descriptor_element_size_bytes(&field_info, ctx.layout)
                                });
                            let component_char_alloc_len = match &field.type_info {
                                crate::sema::symtab::TypeInfo::Character {
                                    len: Some(_), ..
                                } => None,
                                _ => char_alloc_len,
                            };
                            let dynamic_descriptor_elem_size = if field_info.is_class {
                                source_desc
                                    .or(mold_shape_desc)
                                    .map(|desc| descriptor_elem_size(b, desc))
                            } else {
                                None
                            };
                            let es = source_char_elem_size
                                .or(dynamic_descriptor_elem_size)
                                .unwrap_or_else(|| {
                                    allocated_array_elem_size(
                                        b,
                                        &field_info,
                                        elem_size_bytes,
                                        component_char_alloc_len,
                                    )
                                });
                            if field_info.is_class {
                                if let Some(metadata_desc) = shape_desc {
                                    require_context_free_dynamic_lifecycle(b, metadata_desc);
                                }
                            }
                            let one_i64 = b.const_i64(1);
                            let dim_buf = if rank == 0 {
                                b.const_i64(0)
                            } else {
                                let dim_buf_bytes = (rank * 24) as u64;
                                let dim_buf = b.alloca(IrType::Array(
                                    Box::new(IrType::Int(IntWidth::I8)),
                                    dim_buf_bytes,
                                ));
                                for (i, &(lo64, up64)) in bounds.iter().enumerate() {
                                    let base = (i * 24) as i64;
                                    let off_lo = b.const_i64(base);
                                    let off_up = b.const_i64(base + 8);
                                    let off_st = b.const_i64(base + 16);
                                    let p_lo =
                                        b.gep(dim_buf, vec![off_lo], IrType::Int(IntWidth::I8));
                                    let p_up =
                                        b.gep(dim_buf, vec![off_up], IrType::Int(IntWidth::I8));
                                    let p_st =
                                        b.gep(dim_buf, vec![off_st], IrType::Int(IntWidth::I8));
                                    b.store(lo64, p_lo);
                                    b.store(up64, p_up);
                                    b.store(one_i64, p_st);
                                }
                                dim_buf
                            };
                            if rank == 0 {
                                if let Some(shape_desc) = shape_desc {
                                    b.call(
                                        FuncRef::External("afs_allocate_like".into()),
                                        vec![field_ptr, shape_desc, runtime_stat_arg],
                                        IrType::Void,
                                    );
                                } else {
                                    let rank_val = b.const_i32(0);
                                    b.call(
                                        FuncRef::External("afs_allocate_array".into()),
                                        vec![field_ptr, es, rank_val, dim_buf, runtime_stat_arg],
                                        IrType::Void,
                                    );
                                }
                            } else {
                                let rank_val = b.const_i32(rank as i32);
                                b.call(
                                    FuncRef::External("afs_allocate_array".into()),
                                    vec![field_ptr, es, rank_val, dim_buf, runtime_stat_arg],
                                    IrType::Void,
                                );
                            }
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "ALLOCATE failed",
                            );
                            if let Some(source_desc) = source_desc {
                                emit_allocatable_source_copy_on_success(
                                    b,
                                    stat_addr,
                                    runtime_stat_arg,
                                    field_ptr,
                                    source_desc,
                                    source_copy_rank > 0,
                                    array_source_copy_layout,
                                    source_copy_plan.as_ref(),
                                    ctx.type_layouts,
                                    errmsg_target.as_ref(),
                                );
                            } else if rank == 0 {
                                if let Some(source_desc) = source_scalar_desc {
                                    emit_allocatable_source_copy_on_success(
                                        b,
                                        stat_addr,
                                        runtime_stat_arg,
                                        field_ptr,
                                        source_desc,
                                        false,
                                        None,
                                        source_copy_plan.as_ref(),
                                        ctx.type_layouts,
                                        errmsg_target.as_ref(),
                                    );
                                } else if let Some(source_expr) = source_expr {
                                    if !expr_is_character_expr(
                                        b,
                                        &ctx.locals,
                                        source_expr,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                    ) {
                                        let init_ty =
                                            source_intrinsic_elem_ty.as_ref().unwrap_or(&elem_ty);
                                        let dest_base = b.load_typed(
                                            field_ptr,
                                            IrType::Ptr(Box::new(init_ty.clone())),
                                        );
                                        emit_scalar_allocate_source_init_on_success(
                                            b,
                                            ctx,
                                            stat_addr,
                                            dest_base,
                                            init_ty,
                                            source_scalar_type
                                                .as_deref()
                                                .or(field_derived_type_name(&field).as_deref()),
                                            source_expr,
                                        );
                                    } else if field_is_class_star {
                                        if let Some((src_ptr, src_len)) = source_char {
                                            emit_scalar_class_star_char_source_copy_on_success(
                                                b, stat_addr, field_ptr, src_ptr, src_len,
                                            );
                                        }
                                    } else if let Some((src_ptr, src_len)) = source_char {
                                        emit_scalar_fixed_char_source_copy_on_success(
                                            b, stat_addr, field_ptr, src_ptr, src_len,
                                        );
                                    }
                                }
                            } else if let Some(source_expr) = source_expr {
                                if !expr_is_character_expr(
                                    b,
                                    &ctx.locals,
                                    source_expr,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                ) {
                                    let init_ty =
                                        source_intrinsic_elem_ty.as_ref().unwrap_or(&elem_ty);
                                    emit_array_allocate_scalar_source_init_on_success(
                                        b,
                                        ctx,
                                        stat_addr,
                                        field_ptr,
                                        init_ty,
                                        source_scalar_type
                                            .as_deref()
                                            .or(field_derived_type_name(&field).as_deref()),
                                        source_expr,
                                    );
                                }
                            }
                            // Polymorphic metadata (tag + vtable) for both
                            // scalars and arrays; a polymorphic array
                            // component's elements share one dynamic type.
                            {
                                let mold_metadata_desc = if source_desc.is_none()
                                    && source_scalar_desc.is_none()
                                    && source_expr.is_none()
                                    && typed_type_tag.is_none()
                                    && mold_static_type_tag.is_none()
                                    && mold_static_vtable.is_none()
                                {
                                    mold_shape_desc
                                } else {
                                    None
                                };
                                let field_type_name = field_derived_type_name(&field);
                                let type_tag = if source_desc.is_some()
                                    || mold_metadata_desc.is_some()
                                {
                                    None
                                } else if let Some(source_expr) = source_expr {
                                    (if field_is_class_star {
                                        class_star_intrinsic_source_tag_value(b, ctx, source_expr)
                                    } else {
                                        None
                                    })
                                    .or_else(|| {
                                        expr_type_tag_value(
                                            b,
                                            source_expr,
                                            Some(&ctx.locals),
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                    .or_else(|| {
                                        static_alloc_target_type_tag_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                } else if let Some(tag) = mold_static_type_tag {
                                    Some(tag)
                                } else if let Some(tag) = typed_type_tag {
                                    Some(tag)
                                } else {
                                    derived_type_tag_value(
                                        b,
                                        field_type_name.as_deref(),
                                        ctx.type_layouts,
                                    )
                                    .or_else(|| {
                                        static_alloc_target_type_tag_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                };
                                let vtable =
                                    if source_desc.is_some() || mold_metadata_desc.is_some() {
                                        None
                                    } else if let Some(source_expr) = source_expr {
                                        expr_vtable_value(
                                            b,
                                            source_expr,
                                            Some(&ctx.locals),
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                        .or_else(|| {
                                            static_alloc_target_vtable_value(
                                                b,
                                                item,
                                                ctx.st,
                                                ctx.type_layouts,
                                            )
                                        })
                                    } else if let Some(ptr) = mold_static_vtable {
                                        Some(ptr)
                                    } else if let Some(ptr) = typed_vtable {
                                        Some(ptr)
                                    } else {
                                        derived_type_vtable_value(
                                            b,
                                            field_type_name.as_deref(),
                                            ctx.type_layouts,
                                        )
                                        .or_else(|| {
                                            static_alloc_target_vtable_value(
                                                b,
                                                item,
                                                ctx.st,
                                                ctx.type_layouts,
                                            )
                                        })
                                    };
                                emit_scalar_alloc_polymorphic_metadata_on_success(
                                    b, stat_addr, field_ptr, type_tag, vtable,
                                );
                                if let Some(source_desc) = source_desc {
                                    emit_scalar_alloc_source_descriptor_metadata_on_success(
                                        b,
                                        stat_addr,
                                        field_ptr,
                                        source_desc,
                                    );
                                } else if let Some(source_desc) = source_scalar_desc {
                                    emit_scalar_alloc_source_descriptor_metadata_on_success(
                                        b,
                                        stat_addr,
                                        field_ptr,
                                        source_desc,
                                    );
                                } else if let Some(mold_desc) = mold_metadata_desc {
                                    emit_scalar_alloc_source_descriptor_metadata_on_success(
                                        b, stat_addr, field_ptr, mold_desc,
                                    );
                                }
                                let initialized_from_source = source_desc.is_some()
                                    || source_scalar_desc.is_some()
                                    || source_expr.is_some();
                                if !initialized_from_source {
                                    if let Some(layout) = dynamic_layout {
                                        emit_allocatable_default_init_on_success(
                                            b,
                                            stat_addr,
                                            field_ptr,
                                            layout,
                                            source_copy_rank > 0,
                                            ctx.type_layouts,
                                        );
                                    }
                                }
                            }
                            continue;
                        }
                        if (field.allocatable || field.pointer) && args.is_empty() {
                            let field_info = LocalInfo {
                                addr: field_ptr,
                                ty: field_storage_ir_type(&field, ctx.type_layouts),
                                dims: vec![],
                                allocatable: false,
                                descriptor_arg: false,
                                by_ref: false,
                                char_kind: field_char_kind(&field),
                                derived_type: field_derived_type_name(&field),
                                inline_const: None,
                                is_pointer: field.pointer,
                                runtime_dim_upper: vec![],
                                is_class: false,
                                logical_kind: None,
                                last_dim_assumed_size: false,
                            };
                            let elem_size_bytes =
                                local_storage_size_bytes(&field_info, ctx.type_layouts, ctx.layout);
                            let size_val = b.const_i64(elem_size_bytes);
                            b.call(
                                FuncRef::External("afs_allocate_scalar".into()),
                                vec![field_ptr, size_val, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "ALLOCATE failed",
                            );
                            if let Some(type_name) = &field_info.derived_type {
                                if let Some(layout) = ctx.type_layouts.get(type_name) {
                                    emit_allocatable_default_init_on_success(
                                        b,
                                        stat_addr,
                                        field_ptr,
                                        layout,
                                        false,
                                        ctx.type_layouts,
                                    );
                                }
                            }
                            continue;
                        }
                    }
                }
                let (base_name, args): (Option<String>, &[crate::ast::expr::Argument]) =
                    match &item.node {
                        Expr::FunctionCall { callee, args } => (extract_base_name(callee), args),
                        Expr::Name { name } => (Some(name.clone()), &[]),
                        _ => (None, &[]),
                    };
                if let Some(name) = base_name {
                    if let Some(info) = ctx.locals.get(&name.to_lowercase()).cloned() {
                        if matches!(info.char_kind, CharKind::Deferred) {
                            let Some(len_val) = char_alloc_len else {
                                eprintln!(
                                    "armfortas: error: {}:{}: deferred-length character ALLOCATE requires a typed length or SOURCE/MOLD support",
                                    stmt.span.start.line, stmt.span.start.col
                                );
                                let _ = std::io::stderr().flush();
                                std::process::exit(1);
                            };
                            let desc = string_descriptor_addr(b, &info);
                            b.call(
                                FuncRef::External("afs_allocate_string".into()),
                                vec![desc, len_val, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "ALLOCATE failed",
                            );
                            if let Some((src_ptr, src_len)) = source_char {
                                let alloc_stat =
                                    b.load_typed(stat_addr, IrType::Int(IntWidth::I32));
                                let zero_i32 = b.const_i32(0);
                                let alloc_ok = b.icmp(CmpOp::Eq, alloc_stat, zero_i32);
                                let copy_bb = b.create_block("allocate_char_source_copy");
                                let done_bb = b.create_block("allocate_char_source_done");
                                b.cond_branch(alloc_ok, copy_bb, vec![], done_bb, vec![]);
                                b.set_block(copy_bb);
                                b.call(
                                    FuncRef::External("afs_assign_char_deferred".into()),
                                    vec![desc, src_ptr, src_len],
                                    IrType::Void,
                                );
                                b.branch(done_bb, vec![]);
                                b.set_block(done_bb);
                            }
                            continue;
                        }
                        let elem_size_bytes =
                            local_storage_size_bytes(&info, ctx.type_layouts, ctx.layout);

                        if info.allocatable || info.descriptor_arg {
                            let local_is_class_star = is_unlimited_polymorphic_local(&info);
                            let bounds = lower_alloc_bounds_list(b, ctx, args);
                            let rank = bounds.len();
                            let source_scalar_layout = if source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_type_layout(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let source_scalar_type = if source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_derived_type_name(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let source_intrinsic_elem_ty = if local_is_class_star
                                && source_desc.is_none()
                            {
                                source_expr
                                    .and_then(|expr| class_star_intrinsic_source_ir_type(ctx, expr))
                            } else {
                                None
                            };
                            let dynamic_layout = source_scalar_layout
                                .or(mold_static_layout)
                                .or(typed_layout)
                                .or_else(|| {
                                    info.derived_type
                                        .as_deref()
                                        .and_then(|type_name| ctx.type_layouts.get(type_name))
                                });
                            let target_rank = local_declared_rank(&info);
                            let source_copy_rank = rank.max(target_rank);
                            let source_copy_plan = source_expr.and_then(|expr| {
                                expr_scalar_alloc_source_copy_plan(
                                    expr,
                                    &ctx.locals,
                                    ctx.st,
                                    ctx.type_layouts,
                                )
                            });
                            let array_source_copy_layout = if source_desc.is_some() {
                                if source_copy_rank == 0 && source_copy_plan.is_some() {
                                    None
                                } else {
                                    match source_copy_plan.as_ref() {
                                        Some(ScalarAllocSourceCopyPlan::Static(type_name)) => {
                                            ctx.type_layouts.get(type_name).filter(|layout| {
                                                derived_layout_needs_deep_copy(
                                                    layout,
                                                    ctx.type_layouts,
                                                )
                                            })
                                        }
                                        Some(
                                            ScalarAllocSourceCopyPlan::Dynamic(_)
                                            | ScalarAllocSourceCopyPlan::UnlimitedPolymorphic,
                                        ) => None,
                                        None => dynamic_layout.filter(|layout| {
                                            derived_layout_needs_deep_copy(layout, ctx.type_layouts)
                                        }),
                                    }
                                }
                            } else {
                                None
                            };
                            let source_char_elem_size =
                                if local_is_class_star && rank == 0 && source_char.is_some() {
                                    char_alloc_len
                                } else {
                                    None
                                };
                            // Build a stack DimDescriptor[rank] honoring
                            // each subscript's actual (lower, upper) bounds,
                            // then call afs_allocate_array. Descriptor-backed
                            // dummy arrays use the caller-owned descriptor
                            // rather than the local spill slot that holds its
                            // address. Scalar allocatables lower as a rank-0
                            // descriptor allocation.
                            let static_elem_size = allocated_array_elem_size(
                                b,
                                &info,
                                dynamic_layout
                                    .map(|layout| layout.size as i64)
                                    .or_else(|| {
                                        source_intrinsic_elem_ty
                                            .as_ref()
                                            .map(|ty| ir_scalar_byte_size(ty, ctx.layout))
                                    })
                                    .unwrap_or(elem_size_bytes),
                                char_alloc_len,
                            );
                            let dynamic_descriptor_elem_size = if info.is_class {
                                source_desc
                                    .or(mold_shape_desc)
                                    .map(|desc| descriptor_elem_size(b, desc))
                            } else {
                                None
                            };
                            let es = source_char_elem_size
                                .or(dynamic_descriptor_elem_size)
                                .unwrap_or(static_elem_size);
                            let desc = array_descriptor_addr(b, &info);
                            if info.is_class {
                                if let Some(metadata_desc) = shape_desc {
                                    require_context_free_dynamic_lifecycle(b, metadata_desc);
                                }
                            }
                            let one_i64 = b.const_i64(1);
                            let dim_buf = if rank == 0 {
                                b.const_i64(0)
                            } else {
                                let dim_buf_bytes = (rank * 24) as u64;
                                let dim_buf = b.alloca(IrType::Array(
                                    Box::new(IrType::Int(IntWidth::I8)),
                                    dim_buf_bytes,
                                ));
                                for (i, &(lo64, up64)) in bounds.iter().enumerate() {
                                    let base = (i * 24) as i64;
                                    let off_lo = b.const_i64(base);
                                    let off_up = b.const_i64(base + 8);
                                    let off_st = b.const_i64(base + 16);
                                    let p_lo =
                                        b.gep(dim_buf, vec![off_lo], IrType::Int(IntWidth::I8));
                                    let p_up =
                                        b.gep(dim_buf, vec![off_up], IrType::Int(IntWidth::I8));
                                    let p_st =
                                        b.gep(dim_buf, vec![off_st], IrType::Int(IntWidth::I8));
                                    b.store(lo64, p_lo);
                                    b.store(up64, p_up);
                                    b.store(one_i64, p_st);
                                }
                                dim_buf
                            };
                            if rank == 0 {
                                if let Some(shape_desc) = shape_desc {
                                    b.call(
                                        FuncRef::External("afs_allocate_like".into()),
                                        vec![desc, shape_desc, runtime_stat_arg],
                                        IrType::Void,
                                    );
                                } else {
                                    let rank_val = b.const_i32(0);
                                    b.call(
                                        FuncRef::External("afs_allocate_array".into()),
                                        vec![desc, es, rank_val, dim_buf, runtime_stat_arg],
                                        IrType::Void,
                                    );
                                }
                            } else {
                                let rank_val = b.const_i32(rank as i32);
                                b.call(
                                    FuncRef::External("afs_allocate_array".into()),
                                    vec![desc, es, rank_val, dim_buf, runtime_stat_arg],
                                    IrType::Void,
                                );
                            }
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "ALLOCATE failed",
                            );
                            if let Some(source_desc) = source_desc {
                                emit_allocatable_source_copy_on_success(
                                    b,
                                    stat_addr,
                                    runtime_stat_arg,
                                    desc,
                                    source_desc,
                                    source_copy_rank > 0,
                                    array_source_copy_layout,
                                    source_copy_plan.as_ref(),
                                    ctx.type_layouts,
                                    errmsg_target.as_ref(),
                                );
                            } else if rank == 0 {
                                if let Some(source_desc) = source_scalar_desc {
                                    emit_allocatable_source_copy_on_success(
                                        b,
                                        stat_addr,
                                        runtime_stat_arg,
                                        desc,
                                        source_desc,
                                        false,
                                        None,
                                        source_copy_plan.as_ref(),
                                        ctx.type_layouts,
                                        errmsg_target.as_ref(),
                                    );
                                } else if let Some(source_expr) = source_expr {
                                    if !expr_is_character_expr(
                                        b,
                                        &ctx.locals,
                                        source_expr,
                                        ctx.st,
                                        Some(ctx.type_layouts),
                                    ) {
                                        let init_ty =
                                            source_intrinsic_elem_ty.as_ref().unwrap_or(&info.ty);
                                        let dest_base = b.load_typed(
                                            desc,
                                            IrType::Ptr(Box::new(init_ty.clone())),
                                        );
                                        emit_scalar_allocate_source_init_on_success(
                                            b,
                                            ctx,
                                            stat_addr,
                                            dest_base,
                                            init_ty,
                                            source_scalar_type
                                                .as_deref()
                                                .or(info.derived_type.as_deref()),
                                            source_expr,
                                        );
                                    } else if local_is_class_star {
                                        if let Some((src_ptr, src_len)) = source_char {
                                            emit_scalar_class_star_char_source_copy_on_success(
                                                b, stat_addr, desc, src_ptr, src_len,
                                            );
                                        }
                                    } else if let Some((src_ptr, src_len)) = source_char {
                                        emit_scalar_fixed_char_source_copy_on_success(
                                            b, stat_addr, desc, src_ptr, src_len,
                                        );
                                    }
                                }
                            } else if let Some(source_expr) = source_expr {
                                if !expr_is_character_expr(
                                    b,
                                    &ctx.locals,
                                    source_expr,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                ) {
                                    let init_ty =
                                        source_intrinsic_elem_ty.as_ref().unwrap_or(&info.ty);
                                    emit_array_allocate_scalar_source_init_on_success(
                                        b,
                                        ctx,
                                        stat_addr,
                                        desc,
                                        init_ty,
                                        source_scalar_type
                                            .as_deref()
                                            .or(info.derived_type.as_deref()),
                                        source_expr,
                                    );
                                }
                            }
                            // Polymorphic metadata (dynamic type tag and
                            // vtable pointer) is set for both scalars and
                            // arrays — a polymorphic array's elements share
                            // one dynamic type, so one tag + table pointer
                            // in the descriptor serves every element-wise
                            // TBP dispatch.
                            {
                                let mold_metadata_desc = if source_desc.is_none()
                                    && source_scalar_desc.is_none()
                                    && source_expr.is_none()
                                    && typed_type_tag.is_none()
                                    && mold_static_type_tag.is_none()
                                    && mold_static_vtable.is_none()
                                {
                                    mold_shape_desc
                                } else {
                                    None
                                };
                                let type_tag = if source_desc.is_some()
                                    || mold_metadata_desc.is_some()
                                {
                                    None
                                } else if let Some(source_expr) = source_expr {
                                    (if local_is_class_star {
                                        class_star_intrinsic_source_tag_value(b, ctx, source_expr)
                                    } else {
                                        None
                                    })
                                    .or_else(|| {
                                        expr_type_tag_value(
                                            b,
                                            source_expr,
                                            Some(&ctx.locals),
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                    .or_else(|| {
                                        static_alloc_target_type_tag_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                } else if let Some(tag) = mold_static_type_tag {
                                    Some(tag)
                                } else if let Some(tag) = typed_type_tag {
                                    Some(tag)
                                } else {
                                    derived_type_tag_value(
                                        b,
                                        info.derived_type.as_deref(),
                                        ctx.type_layouts,
                                    )
                                    .or_else(|| {
                                        static_alloc_target_type_tag_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                };
                                let vtable =
                                    if source_desc.is_some() || mold_metadata_desc.is_some() {
                                        None
                                    } else if let Some(source_expr) = source_expr {
                                        expr_vtable_value(
                                            b,
                                            source_expr,
                                            Some(&ctx.locals),
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                        .or_else(|| {
                                            static_alloc_target_vtable_value(
                                                b,
                                                item,
                                                ctx.st,
                                                ctx.type_layouts,
                                            )
                                        })
                                    } else if let Some(ptr) = mold_static_vtable {
                                        Some(ptr)
                                    } else if let Some(ptr) = typed_vtable {
                                        Some(ptr)
                                    } else {
                                        derived_type_vtable_value(
                                            b,
                                            info.derived_type.as_deref(),
                                            ctx.type_layouts,
                                        )
                                        .or_else(|| {
                                            static_alloc_target_vtable_value(
                                                b,
                                                item,
                                                ctx.st,
                                                ctx.type_layouts,
                                            )
                                        })
                                    };
                                emit_scalar_alloc_polymorphic_metadata_on_success(
                                    b, stat_addr, desc, type_tag, vtable,
                                );
                                if let Some(source_desc) = source_desc {
                                    emit_scalar_alloc_source_descriptor_metadata_on_success(
                                        b,
                                        stat_addr,
                                        desc,
                                        source_desc,
                                    );
                                } else if let Some(source_desc) = source_scalar_desc {
                                    emit_scalar_alloc_source_descriptor_metadata_on_success(
                                        b,
                                        stat_addr,
                                        desc,
                                        source_desc,
                                    );
                                } else if let Some(mold_desc) = mold_metadata_desc {
                                    emit_scalar_alloc_source_descriptor_metadata_on_success(
                                        b, stat_addr, desc, mold_desc,
                                    );
                                }
                                let initialized_from_source = source_desc.is_some()
                                    || source_scalar_desc.is_some()
                                    || source_expr.is_some();
                                if !initialized_from_source {
                                    if let Some(layout) = dynamic_layout {
                                        emit_allocatable_default_init_on_success(
                                            b,
                                            stat_addr,
                                            desc,
                                            layout,
                                            source_copy_rank > 0,
                                            ctx.type_layouts,
                                        );
                                    }
                                }
                            }
                        } else {
                            // Scalar pointers use raw pointer slots rather than descriptors.
                            let slot = if info.is_pointer && info.by_ref {
                                b.load_typed(
                                    info.addr,
                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                )
                            } else {
                                info.addr
                            };
                            if info.is_pointer {
                                let size_val = b.const_i64(elem_size_bytes);
                                b.call(
                                    FuncRef::External("afs_allocate_scalar".into()),
                                    vec![slot, size_val, runtime_stat_arg],
                                    IrType::Void,
                                );
                                emit_runtime_errmsg_on_failure(
                                    b,
                                    stat_addr,
                                    errmsg_target.as_ref(),
                                    "ALLOCATE failed",
                                );
                            } else {
                                let size_val = b.const_i32(elem_size_bytes as i32);
                                let ptr = b.runtime_call(
                                    RuntimeFunc::Allocate,
                                    vec![size_val],
                                    IrType::Ptr(Box::new(info.ty.clone())),
                                );
                                b.store(ptr, slot);
                            }
                            if let Some(type_name) = &info.derived_type {
                                if let Some(layout) = ctx.type_layouts.get(type_name) {
                                    emit_allocatable_default_init_on_success(
                                        b,
                                        stat_addr,
                                        slot,
                                        layout,
                                        false,
                                        ctx.type_layouts,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(allocate_done_bb, vec![]);
            }
            b.set_block(allocate_done_bb);
            super::core::emit_allocate_status_writeback(b, &stat_target);
        }

        Stmt::Deallocate { items, opts } => {
            let dealloc_stat_target = super::core::allocate_status_target(b, ctx, opts);
            let stat_addr = dealloc_stat_target.runtime_addr;
            let runtime_stat_arg = if allocate_keyword_expr(opts, "stat").is_some() {
                stat_addr
            } else {
                b.const_i64(0)
            };
            let zero_i32 = b.const_i32(0);
            b.store(zero_i32, stat_addr);
            let errmsg_target = allocate_errmsg_target(b, ctx, opts);
            let deallocate_done_bb = b.create_block("deallocate_done");
            for (item_idx, item) in items.iter().enumerate() {
                if item_idx > 0 {
                    let item_stat = b.load_typed(stat_addr, IrType::Int(IntWidth::I32));
                    let zero_i32 = b.const_i32(0);
                    let item_ok = b.icmp(CmpOp::Eq, item_stat, zero_i32);
                    let deallocate_item_bb = b.create_block("deallocate_item");
                    b.cond_branch(
                        item_ok,
                        deallocate_item_bb,
                        vec![],
                        deallocate_done_bb,
                        vec![],
                    );
                    b.set_block(deallocate_item_bb);
                }
                if let Expr::ComponentAccess { .. } = &item.node {
                    if let Some((field_ptr, field_owner, field)) =
                        resolve_component_field_access_with_owner(
                            b,
                            &ctx.locals,
                            item,
                            ctx.st,
                            ctx.type_layouts,
                        )
                    {
                        if is_deferred_char_component_field(&field) {
                            b.call(
                                FuncRef::External("afs_dealloc_string_checked".into()),
                                vec![field_ptr, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "DEALLOCATE failed",
                            );
                            continue;
                        }
                        if field.size == 392 && (field.allocatable || field.pointer) {
                            if field.allocatable {
                                if matches!(
                                    &field.type_info,
                                    crate::sema::symtab::TypeInfo::ClassStar
                                        | crate::sema::symtab::TypeInfo::TypeStar
                                ) {
                                    let finalize = b.const_i32(1);
                                    release_unlimited_polymorphic_allocatable_descriptor_checked(
                                        b,
                                        field_ptr,
                                        stat_addr,
                                        runtime_stat_arg,
                                        finalize,
                                    );
                                    emit_runtime_errmsg_on_failure(
                                        b,
                                        stat_addr,
                                        errmsg_target.as_ref(),
                                        "DEALLOCATE failed",
                                    );
                                    continue;
                                }
                                if matches!(
                                    &field.type_info,
                                    crate::sema::symtab::TypeInfo::Class(_)
                                ) {
                                    require_context_free_dynamic_lifecycle(b, field_ptr);
                                }
                                if let Some(type_name) = field_derived_type_name(&field) {
                                    if let Some(layout) =
                                        ctx.type_layouts.get_related(field_owner, &type_name)
                                    {
                                        finalize_derived_descriptor_storage_if_allocated(
                                            b,
                                            ctx.st,
                                            ctx.internal_funcs,
                                            Some(ctx.contained_host_refs),
                                            &ctx.locals,
                                            field_ptr,
                                            layout,
                                            ctx.type_layouts,
                                        );
                                        deallocate_derived_descriptor_components(
                                            b,
                                            field_ptr,
                                            layout,
                                            ctx.type_layouts,
                                            stat_addr,
                                        );
                                    }
                                }
                            }
                            b.call(
                                FuncRef::External("afs_deallocate_array".into()),
                                vec![field_ptr, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "DEALLOCATE failed",
                            );
                            continue;
                        }
                        if field.pointer {
                            b.call(
                                FuncRef::External("afs_deallocate_pointer".into()),
                                vec![field_ptr, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "DEALLOCATE failed",
                            );
                            continue;
                        }
                    }
                }
                let base_name = extract_base_name(item);
                if let Some(name) = base_name {
                    if let Some(info) = ctx.locals.get(&name.to_lowercase()) {
                        if matches!(info.char_kind, CharKind::Deferred) {
                            let desc = string_descriptor_addr(b, info);
                            b.call(
                                FuncRef::External("afs_dealloc_string_checked".into()),
                                vec![desc, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "DEALLOCATE failed",
                            );
                        } else if info.allocatable || info.descriptor_arg {
                            let desc = array_descriptor_addr(b, info);
                            if is_unlimited_polymorphic_local(info) {
                                let finalize = b.const_i32(1);
                                release_unlimited_polymorphic_allocatable_descriptor_checked(
                                    b,
                                    desc,
                                    stat_addr,
                                    runtime_stat_arg,
                                    finalize,
                                );
                            } else {
                                if info.is_class {
                                    require_context_free_dynamic_lifecycle(b, desc);
                                }
                                if let Some(type_name) = &info.derived_type {
                                    if let Some(layout) = ctx.type_layouts.get(type_name) {
                                        finalize_derived_descriptor_storage_if_allocated(
                                            b,
                                            ctx.st,
                                            ctx.internal_funcs,
                                            Some(ctx.contained_host_refs),
                                            &ctx.locals,
                                            desc,
                                            layout,
                                            ctx.type_layouts,
                                        );
                                        deallocate_derived_descriptor_components(
                                            b,
                                            desc,
                                            layout,
                                            ctx.type_layouts,
                                            stat_addr,
                                        );
                                    }
                                }
                                b.call(
                                    FuncRef::External("afs_deallocate_array".into()),
                                    vec![desc, runtime_stat_arg],
                                    IrType::Void,
                                );
                            }
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "DEALLOCATE failed",
                            );
                        } else if info.is_pointer {
                            let slot = if info.by_ref {
                                b.load(info.addr)
                            } else {
                                info.addr
                            };
                            b.call(
                                FuncRef::External("afs_deallocate_pointer".into()),
                                vec![slot, runtime_stat_arg],
                                IrType::Void,
                            );
                            emit_runtime_errmsg_on_failure(
                                b,
                                stat_addr,
                                errmsg_target.as_ref(),
                                "DEALLOCATE failed",
                            );
                        } else {
                            let ptr = b.load_typed(
                                info.addr,
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            b.runtime_call(RuntimeFunc::Deallocate, vec![ptr], IrType::Void);
                        }
                    }
                }
            }
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(deallocate_done_bb, vec![]);
            }
            b.set_block(deallocate_done_bb);
            super::core::emit_allocate_status_writeback(b, &dealloc_stat_target);
        }

        Stmt::Block {
            name,
            uses,
            implicit,
            decls,
            body,
            ..
        } => {
            let _block_use_guard = BlockUseGuard::enter(uses);
            let _block_scope_guard =
                BlockScopeGuard::enter(ctx.st.statement_block_scope(stmt.span));
            // F2008 BLOCK: declarations are scoped to the body.
            // Save any locals that the BLOCK's decls shadow, run
            // the body, then restore the originals.  F2018 §11.1.4
            // also gives the BLOCK its own implicit-typing
            // environment: an `implicit integer (i-n)` here introduces
            // `n` as an integer local even when the enclosing scope
            // is IMPLICIT NONE.  Synthesise TypeDecl entries for any
            // name the body references that isn't in ctx.locals and
            // whose first letter falls in a block-local implicit
            // range, then run alloc_decls / init_decls over the
            // combined list.
            let pre_block_keys: HashSet<String> = ctx.locals.keys().cloned().collect();
            let mut effective_decls: Vec<crate::ast::decl::SpannedDecl> = decls.clone();
            let mut implicit_map: std::collections::HashMap<char, crate::ast::decl::TypeSpec> =
                std::collections::HashMap::new();
            for d in implicit {
                if let crate::ast::decl::Decl::ImplicitStmt { specs } = &d.node {
                    for spec in specs {
                        for &(start, end) in &spec.ranges {
                            for letter_byte in start as u8..=end as u8 {
                                let letter = (letter_byte as char).to_ascii_lowercase();
                                implicit_map.insert(letter, spec.type_spec.clone());
                            }
                        }
                    }
                }
            }
            if !implicit_map.is_empty() {
                let mut already_decl: std::collections::HashSet<String> = decls
                    .iter()
                    .flat_map(|d| {
                        if let crate::ast::decl::Decl::TypeDecl { entities, .. } = &d.node {
                            entities
                                .iter()
                                .map(|e| e.name.to_lowercase())
                                .collect::<Vec<_>>()
                        } else {
                            vec![]
                        }
                    })
                    .collect();
                let mut referenced: Vec<String> = Vec::new();
                for s in body {
                    collect_referenced_names(s, &mut referenced);
                }
                for name in referenced {
                    let key = name.to_lowercase();
                    if already_decl.contains(&key) {
                        continue;
                    }
                    if ctx.locals.contains_key(&key) {
                        continue;
                    }
                    let Some(first) = key.chars().next() else {
                        continue;
                    };
                    let Some(type_spec) = implicit_map.get(&first.to_ascii_lowercase()) else {
                        continue;
                    };
                    already_decl.insert(key.clone());
                    let synth = crate::ast::decl::Decl::TypeDecl {
                        type_spec: type_spec.clone(),
                        attrs: Vec::new(),
                        entities: vec![crate::ast::decl::EntityDecl {
                            name: name.clone(),
                            array_spec: None,
                            char_len: None,
                            init: None,
                            ptr_init: None,
                        }],
                    };
                    effective_decls.push(crate::ast::Spanned {
                        node: synth,
                        span: stmt.span,
                    });
                }
            }
            let block_keys: Vec<String> = effective_decls
                .iter()
                .flat_map(|d| {
                    if let crate::ast::decl::Decl::TypeDecl { entities, .. } = &d.node {
                        entities
                            .iter()
                            .map(|e| e.name.to_lowercase())
                            .collect::<Vec<_>>()
                    } else {
                        vec![]
                    }
                })
                .collect();
            let mut block_imports = HashMap::new();
            if !uses.is_empty() {
                let required_import_names = collect_required_import_names(&effective_decls, body);
                install_block_globals_as_locals(
                    b,
                    &mut block_imports,
                    ctx.globals,
                    uses,
                    &required_import_names,
                    ctx.st,
                    &ctx.ambiguous_use_warnings,
                );
            }
            let block_key_set: HashSet<&str> = block_keys.iter().map(String::as_str).collect();
            let mut scoped_keys = block_keys.clone();
            scoped_keys.extend(
                block_imports
                    .keys()
                    .filter(|key| !block_key_set.contains(key.as_str()))
                    .cloned(),
            );
            let saved: Vec<(String, Option<LocalInfo>)> = scoped_keys
                .iter()
                .map(|k| (k.clone(), ctx.locals.get(k).cloned()))
                .collect();
            for (key, info) in block_imports {
                if !block_key_set.contains(key.as_str()) {
                    ctx.locals.insert(key, info);
                }
            }
            if !effective_decls.is_empty() {
                // Remove shadowed keys so alloc_decls creates fresh allocas.
                for k in &block_keys {
                    ctx.locals.remove(k);
                }
                super::alloc::alloc_decls(
                    b,
                    &mut ctx.locals,
                    &effective_decls,
                    &HashMap::new(),
                    ctx.type_layouts,
                    &mut Vec::new(),
                    "",
                    ctx.st,
                );
                super::init::init_decls(
                    b,
                    &ctx.locals,
                    &effective_decls,
                    ctx.st,
                    ctx.proc_scope_id,
                    Some(ctx.type_layouts),
                );
            }
            let bb_cleanup = b.create_block("block_cleanup");
            let bb_after = b.create_block("block_after");

            let block_only: HashMap<String, LocalInfo> = block_keys
                .iter()
                .filter(|k| ctx.locals.contains_key(*k))
                .filter_map(|k| ctx.locals.get(k).map(|v| (k.clone(), v.clone())))
                .collect();
            ctx.block_cleanups.push(BlockCleanupScope {
                labels: collect_statement_labels(body),
                owned_locals: block_only,
            });

            ctx.push_construct_exit(name.clone(), bb_cleanup);
            lower_stmts(b, ctx, body);
            ctx.pop_construct_exit(name);
            let block_cleanup = ctx
                .block_cleanups
                .pop()
                .expect("BLOCK cleanup scope must remain active while lowering its body");

            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_cleanup, vec![]);
            }

            b.set_block(bb_cleanup);
            // F2018 §7.5.6.3 / §9.7.3.2: at END BLOCK, finalize derived-type
            // locals that have FINAL subroutines and deallocate block-scoped
            // allocatables. Shadowing declarations own distinct storage and
            // therefore require the same cleanup as uniquely named locals.
            if b.func().block(b.current_block()).terminator.is_none()
                && !block_cleanup.owned_locals.is_empty()
            {
                insert_implicit_dealloc(
                    b,
                    &block_cleanup.owned_locals,
                    &ctx.locals,
                    ctx.type_layouts,
                    ctx.st,
                    ctx.internal_funcs,
                    Some(ctx.contained_host_refs),
                    None,
                    true,
                );
            }
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_after, vec![]);
            }

            b.set_block(bb_after);
            // Restore the outer scope's locals.
            for (k, orig) in saved {
                if let Some(info) = orig {
                    ctx.locals.insert(k, info);
                } else {
                    ctx.locals.remove(&k);
                }
            }
            ctx.locals.retain(|k, _| pre_block_keys.contains(k));
        }

        Stmt::Associate { name, assocs, body } => {
            // Associate names are scoped — they only exist within the body.
            let mut saved = Vec::with_capacity(assocs.len());

            for (assoc_name, expr) in assocs {
                let key = assoc_name.to_lowercase();
                saved.push((key.clone(), ctx.locals.get(&key).cloned()));
                if let Some(info) = associate_alias_local_info(b, ctx, expr) {
                    ctx.locals.insert(key, info);
                    continue;
                }
                let val = super::expr::lower_expr_ctx(b, ctx, expr);
                let ty = b
                    .func()
                    .value_type(val)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let addr = b.alloca(ty.clone());
                b.store(val, addr);
                ctx.locals.insert(
                    key,
                    LocalInfo {
                        addr,
                        ty,
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

            if name.is_some() {
                let bb_after = b.create_block("associate_after");
                ctx.push_construct_exit(name.clone(), bb_after);
                lower_stmts(b, ctx, body);
                ctx.pop_construct_exit(name);

                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(bb_after, vec![]);
                }
                b.set_block(bb_after);
            } else {
                lower_stmts(b, ctx, body);
            }

            for (key, original) in saved.into_iter().rev() {
                if let Some(info) = original {
                    ctx.locals.insert(key, info);
                } else {
                    ctx.locals.remove(&key);
                }
            }
        }

        Stmt::Continue { label: Some(lbl) } => {
            // Labeled CONTINUE: fall through to the label's block.
            if let Some(&label_bb) = ctx.label_blocks.get(lbl) {
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(label_bb, vec![]);
                }
                b.set_block(label_bb);
            }
        }
        Stmt::Continue { label: None } => {} // no-op
        Stmt::Format { .. } => {}            // non-executable metadata

        Stmt::Goto { label } => {
            if let Some(&target_bb) = ctx.label_blocks.get(label) {
                emit_block_cleanups_for_goto(b, ctx, *label);
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(target_bb, vec![]);
                }
            }
        }

        Stmt::ComputedGoto { labels, selector } => {
            // F2018 §11.2.3: `GO TO (l1, l2, ..., ln) expr` evaluates `expr`
            // (integer); if 1 <= expr <= n, branches to label expr; otherwise
            // falls through to the next statement.
            //
            // Lower as a chain of (icmp eq expr, k) branches: for each
            // k = 1..n create (cond_br to labels[k-1], else fallthrough).
            // The fallthrough block is what the next statement sees.
            if labels.is_empty() {
                // Empty list — purely a fall-through with side effect of
                // evaluating the selector. Just lower the expression.
                let _ = super::expr::lower_expr_ctx(b, ctx, selector);
                return;
            }
            let sel_raw = super::expr::lower_expr_ctx(b, ctx, selector);
            let sel_i32 = match b.func().value_type(sel_raw) {
                Some(IrType::Int(IntWidth::I32)) => sel_raw,
                Some(IrType::Int(IntWidth::I64)) => b.int_trunc(sel_raw, IntWidth::I32),
                Some(IrType::Int(_)) => b.int_extend(sel_raw, IntWidth::I32, true),
                _ => sel_raw,
            };
            for (i, label) in labels.iter().enumerate() {
                let Some(&target_bb) = ctx.label_blocks.get(label) else {
                    continue;
                };
                let key = (i + 1) as i32;
                let key_val = b.const_i32(key);
                let matches = b.icmp(CmpOp::Eq, sel_i32, key_val);
                let next_check = b.create_block("computed_goto_next");
                if goto_exits_active_block(ctx, *label) {
                    let cleanup = b.create_block("computed_goto_cleanup");
                    b.cond_branch(matches, cleanup, vec![], next_check, vec![]);
                    b.set_block(cleanup);
                    emit_block_cleanups_for_goto(b, ctx, *label);
                    if b.func().block(b.current_block()).terminator.is_none() {
                        b.branch(target_bb, vec![]);
                    }
                } else {
                    b.cond_branch(matches, target_bb, vec![], next_check, vec![]);
                }
                b.set_block(next_check);
            }
            // Falling out of the loop, current block is the post-chain
            // block — execution continues into whatever statement follows.
        }

        Stmt::Labeled { label, stmt: inner } => {
            // Create an edge from the current block into the label's block (fall-through),
            // then switch to the label's block and lower the inner statement.
            if let Some(&label_bb) = ctx.label_blocks.get(label) {
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(label_bb, vec![]);
                }
                b.set_block(label_bb);
            }
            lower_stmt(b, ctx, inner);
        }

        Stmt::Open { specs } => {
            let unit_spec = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("unit"))
                        .unwrap_or(false)
                })
                .or_else(|| specs.iter().find(|s| s.keyword.is_none()));
            let newunit_spec = specs.iter().find(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("newunit"))
                    .unwrap_or(false)
            });
            let iostat_spec = specs.iter().find(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("iostat"))
                    .unwrap_or(false)
            });
            let iomsg_spec = specs.iter().find(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("iomsg"))
                    .unwrap_or(false)
            });
            let err_label = specs.iter().find_map(|s| {
                if s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("err"))
                    .unwrap_or(false)
                {
                    match &s.value.node {
                        Expr::IntegerLiteral { text, .. } => text.parse::<u64>().ok(),
                        _ => None,
                    }
                } else {
                    None
                }
            });
            let unit = if let Some(s) = unit_spec {
                super::expr::lower_expr_ctx(b, ctx, &s.value)
            } else if newunit_spec.is_some() {
                b.const_i32(0)
            } else {
                b.const_i32(6)
            };

            // Find FILE= spec.
            let (file_ptr, file_len) = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("file"))
                        .unwrap_or(false)
                })
                .map(|s| lower_string_expr_ctx(b, ctx, &s.value))
                .unwrap_or_else(|| {
                    let z = b.const_i64(0);
                    (z, z)
                });

            // Find STATUS= spec.
            let (status_ptr, status_len) = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("status"))
                        .unwrap_or(false)
                })
                .map(|s| lower_string_expr_ctx(b, ctx, &s.value))
                .unwrap_or_else(|| {
                    let z = b.const_i64(0);
                    (z, z)
                });

            // Find ACTION= spec.
            let (action_ptr, action_len) = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("action"))
                        .unwrap_or(false)
                })
                .map(|s| lower_string_expr_ctx(b, ctx, &s.value))
                .unwrap_or_else(|| {
                    let z = b.const_i64(0);
                    (z, z)
                });

            // Find ACCESS= spec.
            let (access_ptr, access_len) = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("access"))
                        .unwrap_or(false)
                })
                .map(|s| lower_string_expr_ctx(b, ctx, &s.value))
                .unwrap_or_else(|| {
                    let z = b.const_i64(0);
                    (z, z)
                });

            // Find FORM= spec.
            let (form_ptr, form_len) = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("form"))
                        .unwrap_or(false)
                })
                .map(|s| lower_string_expr_ctx(b, ctx, &s.value))
                .unwrap_or_else(|| {
                    let z = b.const_i64(0);
                    (z, z)
                });

            // Find RECL= spec.
            let recl_val = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("recl"))
                        .unwrap_or(false)
                })
                .map(|s| super::expr::lower_expr_ctx(b, ctx, &s.value))
                .unwrap_or_else(|| b.const_i64(0));

            let null = b.const_i64(0);
            let null_i8_ptr = b.int_to_ptr(null, IrType::Int(IntWidth::I8));
            let unit_i32 = coerce_to_type(b, unit, &IrType::Int(IntWidth::I32));
            let recl_i64 = coerce_to_type(b, recl_val, &IrType::Int(IntWidth::I64));
            let open_iostat_ptr = match (iostat_spec, err_label) {
                (Some(spec), _) => lower_arg_by_ref_ctx(b, ctx, &spec.value),
                (None, Some(_)) => {
                    let tmp = b.alloca(IrType::Int(IntWidth::I32));
                    let zero = b.const_i32(0);
                    b.store(zero, tmp);
                    tmp
                }
                (None, None) => null,
            };
            let (open_iomsg_ptr, open_iomsg_len) = iomsg_spec
                .map(|spec| lower_string_expr_ctx(b, ctx, &spec.value))
                .unwrap_or((null_i8_ptr, null));
            let mut open_string_temps = Vec::new();
            for ptr in [file_ptr, status_ptr, action_ptr, access_ptr, form_ptr] {
                open_string_temps.extend(b.take_owned_string_temp_bases(ptr));
            }

            // Check if we have any extended specifiers beyond the basic 7-arg set.
            let has_access = specs.iter().any(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("access"))
                    .unwrap_or(false)
            });
            let has_form = specs.iter().any(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("form"))
                    .unwrap_or(false)
            });
            let has_recl = specs.iter().any(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("recl"))
                    .unwrap_or(false)
            });
            let has_position = specs.iter().any(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("position"))
                    .unwrap_or(false)
            });
            let has_leading_zero = specs.iter().any(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("leading_zero"))
                    .unwrap_or(false)
            });
            let has_iostat = iostat_spec.is_some();
            let has_iomsg = iomsg_spec.is_some();
            let has_newunit = newunit_spec.is_some();
            let has_err = err_label.is_some();

            if !has_access
                && !has_form
                && !has_recl
                && !has_position
                && !has_leading_zero
                && !has_iostat
                && !has_iomsg
                && !has_newunit
                && !has_err
            {
                // Simple case: use 7-arg afs_open_simple (unit + 3 string pairs).
                b.call(
                    FuncRef::External("afs_open_simple".into()),
                    vec![
                        unit_i32, file_ptr, file_len, status_ptr, status_len, action_ptr,
                        action_len,
                    ],
                    IrType::Void,
                );
            } else {
                // Extended case: build OpenControlBlock on the stack.
                // Find POSITION= spec.
                let (position_ptr, position_len) = specs
                    .iter()
                    .find(|s| {
                        s.keyword
                            .as_deref()
                            .map(|k| k.eq_ignore_ascii_case("position"))
                            .unwrap_or(false)
                    })
                    .map(|s| lower_string_expr_ctx(b, ctx, &s.value))
                    .unwrap_or_else(|| {
                        let z = b.const_i64(0);
                        (z, z)
                    });

                // Find LEADING_ZERO= spec (F2023 connection-level mode).
                let (leading_zero_ptr, leading_zero_len) = specs
                    .iter()
                    .find(|s| {
                        s.keyword
                            .as_deref()
                            .map(|k| k.eq_ignore_ascii_case("leading_zero"))
                            .unwrap_or(false)
                    })
                    .map(|s| lower_string_expr_ctx(b, ctx, &s.value))
                    .unwrap_or_else(|| {
                        let z = b.const_i64(0);
                        (z, z)
                    });
                open_string_temps.extend(b.take_owned_string_temp_bases(position_ptr));
                open_string_temps.extend(b.take_owned_string_temp_bases(leading_zero_ptr));

                // Layout matches repr(C) OpenControlBlock (160 bytes):
                //   0: unit(i32) + 4 pad, 8: filename(ptr), 16: filename_len(i64),
                //  24: status(ptr), 32: status_len(i64), 40: action(ptr), 48: action_len(i64),
                //  56: access(ptr), 64: access_len(i64), 72: form(ptr), 80: form_len(i64),
                //  88: recl(i64), 96: iostat(ptr), 104: newunit(ptr),
                // 112: position(ptr), 120: position_len(i64),
                // 128: leading_zero(ptr), 136: leading_zero_len(i64),
                // 144: iomsg(ptr), 152: iomsg_len(i64)
                let cb_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 160);
                let cb = b.alloca(cb_ty);

                let store_at = |b: &mut crate::ir::builder::FuncBuilder,
                                base,
                                offset: i64,
                                field_ty: IrType,
                                val| {
                    let field_bytes = field_ty.size_bytes(&b.layout) as i64;
                    debug_assert!(field_bytes > 0 && offset % field_bytes == 0);
                    let slot = b.const_i64(offset / field_bytes);
                    let ptr = b.gep(base, vec![slot], field_ty.clone());
                    let stored = match field_ty {
                        IrType::Int(_) | IrType::Float(_) | IrType::Bool => {
                            coerce_to_type(b, val, &field_ty)
                        }
                        _ => val,
                    };
                    b.store(stored, ptr);
                };

                let file_ptr_ty = b
                    .func()
                    .value_type(file_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let status_ptr_ty = b
                    .func()
                    .value_type(status_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let action_ptr_ty = b
                    .func()
                    .value_type(action_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let access_ptr_ty = b
                    .func()
                    .value_type(access_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let form_ptr_ty = b
                    .func()
                    .value_type(form_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let position_ptr_ty = b
                    .func()
                    .value_type(position_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let leading_zero_ptr_ty = b
                    .func()
                    .value_type(leading_zero_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let iomsg_ptr_ty = b
                    .func()
                    .value_type(open_iomsg_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let newunit_ptr = newunit_spec
                    .map(|spec| lower_arg_by_ref_ctx(b, ctx, &spec.value))
                    .unwrap_or(null);
                let iostat_ptr_ty = b
                    .func()
                    .value_type(open_iostat_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let newunit_ptr_ty = b
                    .func()
                    .value_type(newunit_ptr)
                    .unwrap_or(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));

                store_at(b, cb, 0, IrType::Int(IntWidth::I32), unit_i32);
                store_at(b, cb, 8, file_ptr_ty, file_ptr);
                store_at(b, cb, 16, IrType::Int(IntWidth::I64), file_len);
                store_at(b, cb, 24, status_ptr_ty, status_ptr);
                store_at(b, cb, 32, IrType::Int(IntWidth::I64), status_len);
                store_at(b, cb, 40, action_ptr_ty, action_ptr);
                store_at(b, cb, 48, IrType::Int(IntWidth::I64), action_len);
                store_at(b, cb, 56, access_ptr_ty, access_ptr);
                store_at(b, cb, 64, IrType::Int(IntWidth::I64), access_len);
                store_at(b, cb, 72, form_ptr_ty, form_ptr);
                store_at(b, cb, 80, IrType::Int(IntWidth::I64), form_len);
                store_at(b, cb, 88, IrType::Int(IntWidth::I64), recl_i64);
                store_at(b, cb, 96, iostat_ptr_ty, open_iostat_ptr);
                store_at(b, cb, 104, newunit_ptr_ty, newunit_ptr);
                store_at(b, cb, 112, position_ptr_ty, position_ptr);
                store_at(b, cb, 120, IrType::Int(IntWidth::I64), position_len);
                store_at(b, cb, 128, leading_zero_ptr_ty, leading_zero_ptr);
                store_at(b, cb, 136, IrType::Int(IntWidth::I64), leading_zero_len);
                store_at(b, cb, 144, iomsg_ptr_ty, open_iomsg_ptr);
                store_at(b, cb, 152, IrType::Int(IntWidth::I64), open_iomsg_len);

                b.call(FuncRef::External("afs_open".into()), vec![cb], IrType::Void);
            }
            deallocate_owned_string_bases(b, &open_string_temps);
            lower_read_err_branch(b, ctx, err_label, open_iostat_ptr);
        }

        Stmt::Close { specs } => {
            let unit_spec = specs
                .iter()
                .find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("unit"))
                        .unwrap_or(false)
                })
                .or_else(|| specs.iter().find(|s| s.keyword.is_none()));
            let iostat_spec = specs.iter().find(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("iostat"))
                    .unwrap_or(false)
            });
            let status_spec = specs.iter().find(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("status"))
                    .unwrap_or(false)
            });
            let unit = if let Some(s) = unit_spec {
                super::expr::lower_expr_ctx(b, ctx, &s.value)
            } else {
                b.const_i32(6)
            };
            let null = b.const_i64(0);
            let unit_i32 = coerce_to_type(b, unit, &IrType::Int(IntWidth::I32));
            let iostat_ptr = iostat_spec
                .map(|spec| lower_arg_by_ref_ctx(b, ctx, &spec.value))
                .unwrap_or(null);
            let (status_ptr, status_len) = status_spec
                .map(|spec| lower_string_expr_ctx(b, ctx, &spec.value))
                .unwrap_or_else(|| (null, null));
            b.call(
                FuncRef::External("afs_close_ex".into()),
                vec![unit_i32, status_ptr, status_len, iostat_ptr],
                IrType::Void,
            );
            let status_temps = b.take_owned_string_temp_bases(status_ptr);
            deallocate_owned_string_bases(b, &status_temps);
        }

        Stmt::Read { controls, items } => {
            // `nonadvancing` is the compile-time bool used by the
            // existing per-item helpers; it stays false when the
            // advance= expression is non-literal so the static path
            // calls the advancing helper. `advance_runtime` carries
            // the runtime i32 (0 = no, 1 = yes) for non-literal
            // expressions like `advance=optval(adv,'YES')` from
            // stdlib's read_bitset_unit_64. The char-read helper picks
            // it up via afs_fmt_read_string_dyn.
            let advance_ctrl = controls.iter().find(|c| {
                c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("advance"))
                    .unwrap_or(false)
            });
            let nonadvancing = advance_ctrl
                .and_then(|c| match &c.value.node {
                    Expr::StringLiteral { value, .. } => Some(value.eq_ignore_ascii_case("no")),
                    Expr::Name { name } => Some(name.eq_ignore_ascii_case("no")),
                    _ => None,
                })
                .unwrap_or(false);
            let advance_runtime: Option<ValueId> = advance_ctrl.and_then(|c| {
                if matches!(&c.value.node, Expr::StringLiteral { .. }) {
                    None
                } else {
                    let (p, l) = lower_string_expr_ctx(b, ctx, &c.value);
                    let result = b.call(
                        FuncRef::External("afs_advance_eval".into()),
                        vec![p, l],
                        IrType::Int(IntWidth::I32),
                    );
                    deallocate_owned_string_expr_temp(
                        b,
                        &ctx.locals,
                        &c.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                        p,
                    );
                    Some(result)
                }
            });
            let err_label = controls.iter().find_map(|c| {
                if c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("err"))
                    .unwrap_or(false)
                {
                    match &c.value.node {
                        Expr::IntegerLiteral { text, .. } => text.parse::<u64>().ok(),
                        _ => None,
                    }
                } else {
                    None
                }
            });
            let end_label = controls.iter().find_map(|c| {
                if c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("end"))
                    .unwrap_or(false)
                {
                    match &c.value.node {
                        Expr::IntegerLiteral { text, .. } => text.parse::<u64>().ok(),
                        _ => None,
                    }
                } else {
                    None
                }
            });
            let fmt_control = controls
                .iter()
                .skip(1)
                .find(|c| c.keyword.is_none())
                .or_else(|| {
                    controls.iter().find(|c| {
                        c.keyword
                            .as_deref()
                            .map(|k| k.eq_ignore_ascii_case("fmt"))
                            .unwrap_or(false)
                    })
                });

            let is_list_directed = match fmt_control {
                None => true,
                Some(ctrl) => matches!(&ctrl.value.node, Expr::Name { name } if name == "*"),
            };

            let iomsg_ctrl = controls.iter().find(|c| {
                c.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("iomsg"))
                    .unwrap_or(false)
            });

            let explicit_iostat_addr = controls
                .iter()
                .find(|c| {
                    c.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("iostat"))
                        .unwrap_or(false)
                })
                .map(|c| lower_arg_by_ref_ctx(b, ctx, &c.value));

            let user_iostat = explicit_iostat_addr.is_some();
            let needs_hidden_iostat =
                end_label.is_some() || err_label.is_some() || iomsg_ctrl.is_some();
            let has_dtio_iostat_addr = user_iostat || needs_hidden_iostat;
            let iostat_addr = match explicit_iostat_addr {
                Some(addr) => addr,
                None if needs_hidden_iostat => {
                    let tmp = b.alloca(IrType::Int(IntWidth::I32));
                    let zero = b.const_i32(0);
                    b.store(zero, tmp);
                    tmp
                }
                None => b.const_i64(0),
            };
            let dtio_iostat_addr = has_dtio_iostat_addr.then_some(iostat_addr);

            let size_addr = controls
                .iter()
                .find(|c| {
                    c.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("size"))
                        .unwrap_or(false)
                })
                .map(|c| lower_arg_by_ref_ctx(b, ctx, &c.value))
                .unwrap_or_else(|| b.const_i64(0));
            let null_iomsg_data = {
                let z = b.const_i64(0);
                b.int_to_ptr(z, IrType::Int(IntWidth::I8))
            };
            let zero_iomsg_len = b.const_i64(0);
            let (iomsg_arg_ptr, read_iomsg_ptr, read_iomsg_len) = if let Some(c) = iomsg_ctrl {
                let arg_ptr = lower_arg_by_ref_ctx(b, ctx, &c.value);
                let (ptr, len) = lower_string_expr_ctx(b, ctx, &c.value);
                (arg_ptr, ptr, len)
            } else {
                (null_char_slot_arg(b), null_iomsg_data, zero_iomsg_len)
            };

            if lower_namelist_read_stmt(
                b,
                ctx,
                controls,
                iostat_addr,
                end_label,
                err_label,
                user_iostat,
            ) {
                return;
            }

            lower_read_reset_status(b, iostat_addr);

            if let Some(ctrl) = controls.first() {
                // Whole-char-array internal READ produced silent garbage
                // (the len-0 buffer view class, same as WRITE had). Until
                // the read path grows record-per-element semantics, reject
                // loudly — element units (rec(2)) work and stay routed.
                if internal_io_array_target(b, ctx, ctrl).is_some() {
                    eprintln!(
                        "armfortas: error: internal READ from a whole character array is not implemented; read elements individually"
                    );
                    std::process::exit(1);
                }
                if let Some((buf_ptr, buf_len)) = internal_io_buffer(b, ctx, ctrl) {
                    if is_list_directed {
                        lower_internal_read_items(b, ctx, items, buf_ptr, buf_len, iostat_addr);
                    } else {
                        let (fmt_ptr, fmt_len) =
                            lower_format_expr(b, ctx, &fmt_control.unwrap().value);
                        lower_formatted_internal_read_items(
                            b,
                            ctx,
                            items,
                            buf_ptr,
                            buf_len,
                            fmt_ptr,
                            fmt_len,
                            iostat_addr,
                            size_addr,
                        );
                        deallocate_owned_string_expr_temp(
                            b,
                            &ctx.locals,
                            &fmt_control.unwrap().value,
                            ctx.st,
                            Some(ctx.type_layouts),
                            fmt_ptr,
                        );
                    }
                    lower_read_assign_iomsg(b, iostat_addr, read_iomsg_ptr, read_iomsg_len);
                    lower_read_status_branches(
                        b,
                        ctx,
                        end_label,
                        err_label,
                        iostat_addr,
                        user_iostat,
                    );
                    return;
                }
            }

            // Extract unit (first control). * means stdin (unit 5).
            let unit = if let Some(ctrl) = controls.first() {
                if matches!(&ctrl.value.node, Expr::Name { name } if name == "*") {
                    b.const_i32(5)
                } else {
                    super::expr::lower_expr_ctx(b, ctx, &ctrl.value)
                }
            } else {
                b.const_i32(5) // default stdin
            };
            lower_external_io_pos_seek(b, ctx, controls, unit, iostat_addr);
            let defined_iotype = match fmt_control {
                Some(ctrl) if matches!(&ctrl.value.node, Expr::Name { name } if name == "*") => {
                    Some("LISTDIRECTED")
                }
                Some(_) => Some("DT"),
                None => None,
            };
            if try_lower_defined_io_read_items(
                b,
                ctx,
                items,
                unit,
                defined_iotype,
                dtio_iostat_addr,
                iomsg_arg_ptr,
                read_iomsg_len,
            ) {
                lower_read_status_branches(b, ctx, end_label, err_label, iostat_addr, user_iostat);
                return;
            }
            if is_list_directed {
                // Wrap the per-item reads in begin/end so the runtime
                // can slurp a sequential-unformatted record up front
                // and let the typed helpers consume binary bytes.
                // Formatted units pass through (begin only resets
                // iostat).
                b.call(
                    FuncRef::External("afs_list_read_begin".into()),
                    vec![unit, iostat_addr, read_iomsg_ptr, read_iomsg_len],
                    IrType::Void,
                );
                lower_list_read_items(b, ctx, items, unit, iostat_addr);
                b.call(
                    FuncRef::External("afs_list_read_end".into()),
                    vec![unit, iostat_addr, read_iomsg_ptr, read_iomsg_len],
                    IrType::Void,
                );
            } else {
                let (fmt_ptr, fmt_len) = lower_format_expr(b, ctx, &fmt_control.unwrap().value);
                lower_formatted_read_items_with_runtime_advance(
                    b,
                    ctx,
                    items,
                    unit,
                    fmt_ptr,
                    fmt_len,
                    nonadvancing,
                    advance_runtime,
                    iostat_addr,
                    size_addr,
                );
                deallocate_owned_string_expr_temp(
                    b,
                    &ctx.locals,
                    &fmt_control.unwrap().value,
                    ctx.st,
                    Some(ctx.type_layouts),
                    fmt_ptr,
                );
            }
            lower_read_assign_iomsg(b, iostat_addr, read_iomsg_ptr, read_iomsg_len);
            lower_read_status_branches(b, ctx, end_label, err_label, iostat_addr, user_iostat);
        }

        Stmt::Inquire { specs, .. } => {
            let null = b.const_i64(0);
            let zero_len = b.const_i64(0);
            let spec_by_keyword = |needle: &str| {
                specs.iter().find(|s| {
                    s.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case(needle))
                        .unwrap_or(false)
                })
            };
            let file_spec = specs.iter().find(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("file"))
                    .unwrap_or(false)
            });
            let unit_spec = specs.iter().find(|s| {
                s.keyword
                    .as_deref()
                    .map(|k| k.eq_ignore_ascii_case("unit"))
                    .unwrap_or(false)
            });
            let positional_unit_spec = if file_spec.is_none() {
                specs.iter().find(|s| s.keyword.is_none())
            } else {
                None
            };
            let unit_spec = unit_spec.or(positional_unit_spec);

            let lower_ref_spec = |b: &mut FuncBuilder, needle: &str| -> ValueId {
                spec_by_keyword(needle)
                    .map(|spec| lower_arg_by_ref_ctx(b, ctx, &spec.value))
                    .unwrap_or(null)
            };
            let lower_string_spec = |b: &mut FuncBuilder, needle: &str| -> (ValueId, ValueId) {
                if let Some(spec) = spec_by_keyword(needle) {
                    lower_string_expr_ctx(b, ctx, &spec.value)
                } else {
                    (null, zero_len)
                }
            };

            let exist_addr = lower_ref_spec(b, "exist");
            let opened_addr = lower_ref_spec(b, "opened");
            let iostat_addr = lower_ref_spec(b, "iostat");
            let (name_ptr, name_len) = lower_string_spec(b, "name");
            let (access_ptr, access_len) = lower_string_spec(b, "access");
            let (form_ptr, form_len) = lower_string_spec(b, "form");
            let (action_ptr, action_len) = lower_string_spec(b, "action");
            let (read_ptr, read_len) = lower_string_spec(b, "read");
            let (write_ptr, write_len) = lower_string_spec(b, "write");
            let (readwrite_ptr, readwrite_len) = lower_string_spec(b, "readwrite");
            let (sequential_ptr, sequential_len) = lower_string_spec(b, "sequential");
            let (direct_ptr, direct_len) = lower_string_spec(b, "direct");
            let (stream_ptr, stream_len) = lower_string_spec(b, "stream");
            let (formatted_ptr, formatted_len) = lower_string_spec(b, "formatted");
            let (unformatted_ptr, unformatted_len) = lower_string_spec(b, "unformatted");
            let (leading_zero_ptr, leading_zero_len) = lower_string_spec(b, "leading_zero");
            let recl_spec = spec_by_keyword("recl");
            let (recl_addr, recl_storeback) = if let Some(spec) = recl_spec {
                let dest_addr = lower_arg_by_ref_ctx(b, ctx, &spec.value);
                let dest_ty = inquire_integer_storeback_type(b, ctx, &spec.value, dest_addr);
                let temp = b.alloca(IrType::Int(IntWidth::I64));
                let current = b.load(dest_addr);
                let widened = coerce_to_type(b, current, &IrType::Int(IntWidth::I64));
                b.store(widened, temp);
                (temp, Some((dest_addr, dest_ty)))
            } else {
                (null, None)
            };
            let size_spec = spec_by_keyword("size");
            let (size_addr, size_storeback) = if let Some(spec) = size_spec {
                let dest_addr = lower_arg_by_ref_ctx(b, ctx, &spec.value);
                let temp = b.alloca(IrType::Int(IntWidth::I64));
                let dest_ty = inquire_integer_storeback_type(b, ctx, &spec.value, dest_addr);
                (temp, Some((dest_addr, dest_ty)))
            } else {
                (null, None)
            };
            // POS= (F2018 §12.10.2.22): next file storage unit for a
            // stream unit, 1-based. Same i64-temp + typed-storeback shape
            // as SIZE=. Previously this specifier was silently dropped and
            // the destination kept whatever it held — tomlf's
            // read_whole_file sized its buffer from that garbage.
            let pos_spec = spec_by_keyword("pos");
            let (pos_addr, pos_storeback) = if let Some(spec) = pos_spec {
                let dest_addr = lower_arg_by_ref_ctx(b, ctx, &spec.value);
                let temp = b.alloca(IrType::Int(IntWidth::I64));
                let dest_ty = inquire_integer_storeback_type(b, ctx, &spec.value, dest_addr);
                (temp, Some((dest_addr, dest_ty)))
            } else {
                (null, None)
            };

            if let Some(fs) = file_spec {
                let (fptr, flen) = lower_string_expr_ctx(b, ctx, &fs.value);
                b.call(
                    FuncRef::External("afs_inquire_file".into()),
                    vec![
                        fptr,
                        flen,
                        exist_addr,
                        opened_addr,
                        iostat_addr,
                        name_ptr,
                        name_len,
                        access_ptr,
                        access_len,
                        form_ptr,
                        form_len,
                        action_ptr,
                        action_len,
                        recl_addr,
                        size_addr,
                        pos_addr,
                        read_ptr,
                        read_len,
                        write_ptr,
                        write_len,
                        readwrite_ptr,
                        readwrite_len,
                        sequential_ptr,
                        sequential_len,
                        direct_ptr,
                        direct_len,
                        stream_ptr,
                        stream_len,
                        formatted_ptr,
                        formatted_len,
                        unformatted_ptr,
                        unformatted_len,
                        leading_zero_ptr,
                        leading_zero_len,
                    ],
                    IrType::Void,
                );
                deallocate_owned_string_expr_temp(
                    b,
                    &ctx.locals,
                    &fs.value,
                    ctx.st,
                    Some(ctx.type_layouts),
                    fptr,
                );
            } else if let Some(us) = unit_spec {
                let unit_raw = super::expr::lower_expr_ctx(b, ctx, &us.value);
                let unit = coerce_to_type(b, unit_raw, &IrType::Int(IntWidth::I32));
                b.call(
                    FuncRef::External("afs_inquire_unit".into()),
                    vec![
                        unit,
                        exist_addr,
                        opened_addr,
                        iostat_addr,
                        name_ptr,
                        name_len,
                        access_ptr,
                        access_len,
                        form_ptr,
                        form_len,
                        action_ptr,
                        action_len,
                        recl_addr,
                        size_addr,
                        pos_addr,
                        read_ptr,
                        read_len,
                        write_ptr,
                        write_len,
                        readwrite_ptr,
                        readwrite_len,
                        sequential_ptr,
                        sequential_len,
                        direct_ptr,
                        direct_len,
                        stream_ptr,
                        stream_len,
                        formatted_ptr,
                        formatted_len,
                        unformatted_ptr,
                        unformatted_len,
                        leading_zero_ptr,
                        leading_zero_len,
                    ],
                    IrType::Void,
                );
            }
            if let Some((dest_addr, dest_ty)) = recl_storeback {
                let recl_val = b.load(recl_addr);
                let coerced = coerce_to_type(b, recl_val, &dest_ty);
                b.store(coerced, dest_addr);
            }
            if let Some((dest_addr, dest_ty)) = size_storeback {
                let size_val = b.load(size_addr);
                let coerced = coerce_to_type(b, size_val, &dest_ty);
                b.store(coerced, dest_addr);
            }
            if let Some((dest_addr, dest_ty)) = pos_storeback {
                let pos_val = b.load(pos_addr);
                let coerced = coerce_to_type(b, pos_val, &dest_ty);
                b.store(coerced, dest_addr);
            }
        }

        Stmt::Flush { specs } => {
            let unit = if let Some(s) = specs.first() {
                super::expr::lower_expr_ctx(b, ctx, &s.value)
            } else {
                b.const_i32(6)
            };
            let null = b.const_i64(0);
            b.call(
                FuncRef::External("afs_flush".into()),
                vec![unit, null],
                IrType::Void,
            );
        }

        Stmt::Rewind { specs } => {
            let unit = if let Some(s) = specs.first() {
                super::expr::lower_expr_ctx(b, ctx, &s.value)
            } else {
                b.const_i32(6)
            };
            let null = b.const_i64(0);
            b.call(
                FuncRef::External("afs_rewind".into()),
                vec![unit, null],
                IrType::Void,
            );
        }

        Stmt::Nullify { items } => {
            // Zero each pointer slot so ASSOCIATED returns false.
            for item in items {
                if let Some((field_ptr, field)) =
                    resolve_component_field_access(b, &ctx.locals, item, ctx.st, ctx.type_layouts)
                {
                    if !field.pointer {
                        continue;
                    }
                    let zero_byte = b.const_i32(0);
                    let sz = b.const_i64(field.size as i64);
                    b.call(
                        FuncRef::External("memset".into()),
                        vec![field_ptr, zero_byte, sz],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    continue;
                }
                let Expr::Name { name } = &item.node else {
                    continue;
                };
                let Some(info) = ctx.locals.get(&name.to_lowercase()) else {
                    continue;
                };
                if !info.is_pointer {
                    continue;
                }
                // Array pointers use the 392-byte descriptor, scalar
                // deferred-length character pointers use a 32-byte
                // string descriptor, and scalar pointers use an 8-byte
                // slot. Pointer dummies passed by reference must write
                // through to the caller-owned slot.
                let size = if matches!(info.char_kind, CharKind::Deferred) {
                    32i64
                } else if info.allocatable || info.descriptor_arg {
                    384i64
                } else {
                    8i64
                };
                let slot = if info.by_ref {
                    b.load(info.addr)
                } else {
                    info.addr
                };
                let zero_byte = b.const_i32(0);
                let sz = b.const_i64(size);
                b.call(
                    FuncRef::External("memset".into()),
                    vec![slot, zero_byte, sz],
                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                );
            }
        }

        Stmt::PointerAssignment { target, value } => {
            // `p => q` or `p => x`: rebind the pointer slot `p` to the
            // address of the RHS designator.  Three shapes:
            //
            //   * scalar + derived-type pointer: slot holds an 8-byte
            //     pointer, `=>` stores the target's address into it.
            //   * array pointer: slot holds a 392-byte ArrayDescriptor,
            //     `=>` materialises a descriptor of the target and
            //     memcpy's it into the slot.
            //
            // In both cases the target must be a simple Name for now;
            // component-access and slice targets are follow-up work.

            // Rank-remapping pointer assignment, F2018 §10.2.2.3:
            //   `xmat(1:n, 1:nrhs) => x`   (1-D `x` reinterpreted as 2-D)
            // The LHS is a FunctionCall in the AST whose subscripts are
            // Range bounds, not array indices. The previous fall-through
            // path treated this as scalar pointer assignment and never
            // populated the destination descriptor — `xmat(i,j)` then
            // tripped a bounds check against `[1, 0]`. stdlib_linalg's
            // solve/chol/eig/inverse/svd/norm all do this; ~25 stdlib
            // examples were blocked.
            if let Expr::FunctionCall { callee, args } = &target.node {
                if let Expr::Name { name: tgt_name } = &callee.node {
                    let tgt_key = tgt_name.to_lowercase();
                    let is_remap_target = ctx
                        .locals
                        .get(&tgt_key)
                        .map(|info| info.is_pointer && local_uses_array_descriptor(info))
                        .unwrap_or(false);
                    // F2023 10.2.2.2: `q([2,3]) => t` gives all upper bounds
                    // as one array constructor. Rewrite to per-dimension
                    // `1:ub` ranges so the rank-remap path below handles it.
                    let remap_args = remap_bounds_args(args);
                    let all_ranges = !remap_args.is_empty()
                        && remap_args.iter().all(|a| {
                            matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. })
                        });
                    if is_remap_target && all_ranges {
                        if lower_bounds_remap_pointer_assignment(
                            b,
                            ctx,
                            &tgt_key,
                            &remap_args,
                            value,
                        ) || lower_rank_remap_pointer_assignment(
                            b,
                            ctx,
                            &tgt_key,
                            &remap_args,
                            value,
                        ) {
                            return;
                        }
                        eprintln!(
                            "armfortas: error: {}:{}: pointer bounds remapping shape is not implemented yet",
                            stmt.span.start.line, stmt.span.start.col
                        );
                        let _ = std::io::stderr().flush();
                        std::process::exit(1);
                    }
                }
            }

            let component_target =
                resolve_component_field_access(b, &ctx.locals, target, ctx.st, ctx.type_layouts)
                    .filter(|(_, field)| field.pointer);

            if let Some((tgt_field_ptr, tgt_field)) = component_target.as_ref() {
                if !tgt_field.pointer {
                    return;
                }
                if tgt_field.procedure_pointer {
                    if let Expr::FunctionCall { callee, .. } = &value.node {
                        if let Expr::Name { name } = &callee.node {
                            if name.eq_ignore_ascii_case("null") {
                                let zero = b.const_i64(0);
                                let null = b.int_to_ptr(zero, IrType::Int(IntWidth::I8));
                                store_procedure_pointer_component_record(
                                    b,
                                    *tgt_field_ptr,
                                    null,
                                    &[],
                                );
                                return;
                            }
                        }
                    }
                    if let Expr::Name { name: src_name } = &value.node {
                        let src_key = src_name.to_lowercase();
                        if let Some(src_info) = ctx.locals.get(&src_key) {
                            let closure_args =
                                procedure_dummy_closure_args_from_locals(b, &ctx.locals, &src_key);
                            if !closure_args.is_empty() {
                                let load_ty = if src_info.ty.is_ptr() {
                                    src_info.ty.clone()
                                } else {
                                    IrType::Ptr(Box::new(src_info.ty.clone()))
                                };
                                let addr = b.load_typed(src_info.addr, load_ty);
                                store_procedure_pointer_component_record(
                                    b,
                                    *tgt_field_ptr,
                                    addr,
                                    &closure_args,
                                );
                                return;
                            }
                        }
                        if let Some(sym) = ctx.st.lookup_local_then_any(ctx.proc_scope_id, &src_key)
                        {
                            if matches!(
                                sym.kind,
                                crate::sema::symtab::SymbolKind::Function
                                    | crate::sema::symtab::SymbolKind::Subroutine
                                    | crate::sema::symtab::SymbolKind::ExternalProc
                                    | crate::sema::symtab::SymbolKind::ProcedurePointer
                            ) {
                                let (link_name, resolved_key) =
                                    resolved_symbol_call_target(ctx.st, &src_key, src_name);
                                let lowered_name = if ctx.internal_funcs.contains_key(&resolved_key)
                                    || ctx.internal_funcs.contains_key(&src_key)
                                {
                                    lowered_procedure_symbol_name(
                                        resolved_key.as_str(),
                                        None,
                                        Some(b.func().name.as_str()),
                                        None,
                                        true,
                                        ctx.internal_funcs,
                                    )
                                } else {
                                    link_name
                                };
                                let addr = b.global_addr(&lowered_name, IrType::Int(IntWidth::I8));
                                let mut closure_args = procedure_dummy_closure_args_from_locals(
                                    b,
                                    &ctx.locals,
                                    &src_key,
                                );
                                if closure_args.is_empty() {
                                    append_host_closure_args(
                                        b,
                                        ctx,
                                        if ctx.contained_host_refs.contains_key(&resolved_key) {
                                            &resolved_key
                                        } else {
                                            &src_key
                                        },
                                        &mut closure_args,
                                    );
                                }
                                store_procedure_pointer_component_record(
                                    b,
                                    *tgt_field_ptr,
                                    addr,
                                    &closure_args,
                                );
                                return;
                            }
                        }
                    }
                }
                if is_deferred_char_component_field(tgt_field) {
                    if let Expr::FunctionCall { callee, .. } = &value.node {
                        if let Expr::Name { name } = &callee.node {
                            if name.eq_ignore_ascii_case("null") {
                                let zero = b.const_i64(0);
                                let null = b.int_to_ptr(zero, IrType::Int(IntWidth::I8));
                                store_string_descriptor_view(b, *tgt_field_ptr, null, zero);
                                return;
                            }
                        }
                    }
                    if let Expr::FunctionCall { callee, args } = &value.node {
                        if let Expr::FunctionCall {
                            callee: inner_callee,
                            args: inner_args,
                        } = &callee.node
                        {
                            if let Expr::Name { name: arr_name } = &inner_callee.node {
                                let akey = arr_name.to_lowercase();
                                if let Some(info) = ctx.locals.get(&akey) {
                                    if matches!(info.char_kind, CharKind::Fixed(_))
                                        && (!info.dims.is_empty() || info.allocatable)
                                        && args.len() == 1
                                    {
                                        if let crate::ast::expr::SectionSubscript::Range {
                                            ref start,
                                            ref end,
                                            ..
                                        } = args[0].value
                                        {
                                            let elem_slot = lower_array_element_addr(
                                                b,
                                                &ctx.locals,
                                                info,
                                                inner_args,
                                                ctx.st,
                                                Some(ctx.type_layouts),
                                            );
                                            let zero = b.const_i64(0);
                                            let elem_ptr = b.gep(
                                                elem_slot,
                                                vec![zero],
                                                IrType::Int(IntWidth::I8),
                                            );
                                            let elem_len = match info.char_kind {
                                                CharKind::Fixed(n) => b.const_i64(n),
                                                _ => b.const_i64(0),
                                            };
                                            let (ptr, len) = lower_substring_full(
                                                b,
                                                &ctx.locals,
                                                ctx.st,
                                                elem_ptr,
                                                elem_len,
                                                start.as_ref(),
                                                end.as_ref(),
                                                Some(ctx.type_layouts),
                                                Some(ctx.internal_funcs),
                                                Some(ctx.contained_host_refs),
                                                Some(ctx.descriptor_params),
                                            );
                                            store_string_pointer_descriptor_view(
                                                b,
                                                *tgt_field_ptr,
                                                ptr,
                                                len,
                                            );
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some((src_field_ptr, src_field)) = resolve_component_field_access(
                        b,
                        &ctx.locals,
                        value,
                        ctx.st,
                        ctx.type_layouts,
                    ) {
                        if is_deferred_char_component_field(&src_field) {
                            let (ptr, len) = load_string_descriptor_view(b, src_field_ptr);
                            store_string_pointer_descriptor_view(b, *tgt_field_ptr, ptr, len);
                            return;
                        }
                    }
                    let (ptr, len) = lower_string_expr_ctx(b, ctx, value);
                    store_string_pointer_descriptor_view(b, *tgt_field_ptr, ptr, len);
                    return;
                }
                if field_is_class_star_pointer_descriptor(tgt_field) {
                    let null_value = if let Expr::FunctionCall { callee, .. } = &value.node {
                        if let Expr::Name { name } = &callee.node {
                            name.eq_ignore_ascii_case("null")
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    let desc = if null_value {
                        let zero = b.const_i64(0);
                        b.int_to_ptr(zero, IrType::Int(IntWidth::I8))
                    } else if let Some(desc) = match &value.node {
                        Expr::ComponentAccess { .. } => {
                            class_star_pointer_component_descriptor_value(
                                b,
                                &ctx.locals,
                                value,
                                ctx.st,
                                ctx.type_layouts,
                            )
                        }
                        _ => None,
                    } {
                        desc
                    } else if let Expr::Name { name: src_name } = &value.node {
                        let src_key = src_name.to_lowercase();
                        if let Some(src_info) = ctx.locals.get(&src_key) {
                            if local_uses_array_descriptor(src_info)
                                && local_declared_rank(src_info) == 0
                                && src_info.is_class
                            {
                                array_descriptor_addr(b, src_info)
                            } else {
                                box_actual_into_class_star_descriptor(
                                    b,
                                    &ctx.locals,
                                    value,
                                    ctx.st,
                                    Some(ctx.type_layouts),
                                )
                            }
                        } else {
                            box_actual_into_class_star_descriptor(
                                b,
                                &ctx.locals,
                                value,
                                ctx.st,
                                Some(ctx.type_layouts),
                            )
                        }
                    } else {
                        box_actual_into_class_star_descriptor(
                            b,
                            &ctx.locals,
                            value,
                            ctx.st,
                            Some(ctx.type_layouts),
                        )
                    };
                    store_byte_aggregate_field(
                        b,
                        *tgt_field_ptr,
                        0,
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        desc,
                    );
                    return;
                }
            }

            let Some(tgt_info) = component_target
                .map(|(tgt_field_ptr, tgt_field)| LocalInfo {
                    addr: tgt_field_ptr,
                    ty: if tgt_field.pointer
                        && matches!(
                            tgt_field.type_info,
                            crate::sema::symtab::TypeInfo::Derived(_)
                        )
                        && tgt_field.size != 392
                    {
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))
                    } else {
                        field_storage_ir_type(&tgt_field, ctx.type_layouts)
                    },
                    dims: vec![],
                    allocatable: tgt_field.size == 392
                        && (tgt_field.allocatable || tgt_field.pointer),
                    descriptor_arg: false,
                    by_ref: false,
                    char_kind: field_char_kind(&tgt_field),
                    derived_type: field_derived_type_name(&tgt_field),
                    inline_const: None,
                    is_pointer: tgt_field.pointer,
                    runtime_dim_upper: vec![],
                    is_class: false,
                    logical_kind: None,
                    last_dim_assumed_size: false,
                })
                .or_else(|| {
                    if let Expr::Name { name: tgt_name } = &target.node {
                        ctx.locals.get(&tgt_name.to_lowercase()).cloned()
                    } else {
                        None
                    }
                })
            else {
                return;
            };
            if !tgt_info.is_pointer {
                return;
            }

            if let Expr::FunctionCall { callee, .. } = &value.node {
                if let Expr::Name { name } = &callee.node {
                    if name.eq_ignore_ascii_case("null") {
                        if matches!(tgt_info.char_kind, CharKind::Deferred) {
                            let tgt_desc = string_descriptor_addr(b, &tgt_info);
                            let zero = b.const_i64(0);
                            let null = b.int_to_ptr(zero, IrType::Int(IntWidth::I8));
                            store_string_descriptor_view(b, tgt_desc, null, zero);
                            return;
                        }
                        let zero_byte = b.const_i32(0);
                        let size = if local_uses_array_descriptor(&tgt_info) {
                            384i64
                        } else {
                            8i64
                        };
                        let tgt_slot = if local_uses_array_descriptor(&tgt_info) {
                            array_descriptor_addr(b, &tgt_info)
                        } else if tgt_info.by_ref {
                            b.load(tgt_info.addr)
                        } else {
                            tgt_info.addr
                        };
                        let size_val = b.const_i64(size);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![tgt_slot, zero_byte, size_val],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );
                        return;
                    }
                }
            }

            if matches!(tgt_info.char_kind, CharKind::Deferred) {
                let tgt_desc = string_descriptor_addr(b, &tgt_info);
                if let Expr::FunctionCall { callee, .. } = &value.node {
                    if let Expr::Name { name } = &callee.node {
                        if name.eq_ignore_ascii_case("null") {
                            let zero = b.const_i64(0);
                            let null = b.int_to_ptr(zero, IrType::Int(IntWidth::I8));
                            store_string_descriptor_view(b, tgt_desc, null, zero);
                            return;
                        }
                    }
                }
                if let Some((src_field_ptr, src_field)) =
                    resolve_component_field_access(b, &ctx.locals, value, ctx.st, ctx.type_layouts)
                {
                    if is_deferred_char_component_field(&src_field) {
                        let (ptr, len) = load_string_descriptor_view(b, src_field_ptr);
                        store_string_pointer_descriptor_view(b, tgt_desc, ptr, len);
                        return;
                    }
                }
                if let Expr::Name { name: src_name } = &value.node {
                    if let Some(src_info) = ctx.locals.get(&src_name.to_lowercase()) {
                        if matches!(src_info.char_kind, CharKind::Deferred) {
                            let src_desc = string_descriptor_addr(b, src_info);
                            let (ptr, len) = load_string_descriptor_view(b, src_desc);
                            store_string_pointer_descriptor_view(b, tgt_desc, ptr, len);
                            return;
                        }
                    }
                }
                if let Some(src_desc) = lower_hidden_character_result_descriptor_ctx(b, ctx, value)
                {
                    let size = b.const_i64(32);
                    b.call(
                        FuncRef::External("memcpy".into()),
                        vec![tgt_desc, src_desc, size],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    return;
                }
                let (ptr, len) = lower_string_expr_ctx(b, ctx, value);
                store_string_pointer_descriptor_view(b, tgt_desc, ptr, len);
                return;
            }

            // Handle section-RHS: pa => ia(lo:hi:stride). Reuse the
            // section descriptor builder so extent and memory stride stay
            // tied to the actual section triplet.
            if let Expr::FunctionCall {
                callee,
                args: val_args,
            } = &value.node
            {
                if let Expr::Name { name: arr_name } = &callee.node {
                    let arr_key = arr_name.to_lowercase();
                    if let Some(arr_info) = ctx.locals.get(&arr_key).cloned() {
                        if local_uses_array_descriptor(&tgt_info)
                            && (!arr_info.dims.is_empty()
                                || arr_info.allocatable
                                || arr_info.descriptor_arg)
                            && val_args.iter().any(|arg| {
                                matches!(
                                    arg.value,
                                    crate::ast::expr::SectionSubscript::Range { .. }
                                )
                            })
                        {
                            let src_desc = lower_array_section(
                                b,
                                &ctx.locals,
                                &arr_info,
                                val_args,
                                ctx.st,
                                Some(ctx.type_layouts),
                            );
                            let tgt_desc = array_descriptor_addr(b, &tgt_info);
                            let size = b.const_i64(392);
                            b.call(
                                FuncRef::External("memcpy".into()),
                                vec![tgt_desc, src_desc, size],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            return;
                        }
                    }
                }
            }

            // Array element target: p => arr(i) — compute the
            // element's address via GEP and store into the pointer slot.
            if let Expr::FunctionCall {
                callee,
                args: val_args,
            } = &value.node
            {
                if let Expr::Name { name: arr_name } = &callee.node {
                    let arr_key = arr_name.to_lowercase();
                    if let Some(arr_info) = ctx.locals.get(&arr_key).cloned() {
                        if (!arr_info.dims.is_empty() || arr_info.allocatable)
                            && val_args.len() == 1
                            && matches!(
                                val_args[0].value,
                                crate::ast::expr::SectionSubscript::Element(_)
                            )
                        {
                            if let crate::ast::expr::SectionSubscript::Element(idx_expr) =
                                &val_args[0].value
                            {
                                let base = array_data_ptr_for_call(b, &arr_info);
                                let idx = super::expr::lower_expr_ctx(b, ctx, idx_expr);
                                let idx64 = match b.func().value_type(idx) {
                                    Some(IrType::Int(IntWidth::I64)) => idx,
                                    _ => b.int_extend(idx, IntWidth::I64, true),
                                };
                                let one = b.const_i64(1);
                                let idx0 = b.isub(idx64, one);
                                let elem_ptr = b.gep(base, vec![idx0], arr_info.ty.clone());
                                store_scalar_pointer_slot_value(b, &tgt_info, elem_ptr);
                                return;
                            }
                        }
                    }
                }
            }

            // Component access target: p => dt%field — resolve the
            // field's address and store into the pointer slot.
            if let Expr::ComponentAccess { base, component } = &value.node {
                if let Some((base_addr, type_name)) =
                    resolve_component_base(b, &ctx.locals, base, ctx.st, ctx.type_layouts)
                {
                    if let Some(layout) = ctx.type_layouts.get(&type_name) {
                        if let Some(field) = layout.field(component) {
                            let offset = b.const_i64(field.offset as i64);
                            let field_ptr =
                                b.gep(base_addr, vec![offset], IrType::Int(IntWidth::I8));
                            if is_deferred_char_component_field(field) {
                                let (ptr, _len) = load_string_descriptor_view(b, field_ptr);
                                store_scalar_pointer_slot_value(b, &tgt_info, ptr);
                                return;
                            }
                            if field.size == 392 && (field.pointer || field.allocatable) {
                                if local_uses_array_descriptor(&tgt_info) {
                                    let tgt_desc = array_descriptor_addr(b, &tgt_info);
                                    let size = b.const_i64(392);
                                    b.call(
                                        FuncRef::External("memcpy".into()),
                                        vec![tgt_desc, field_ptr, size],
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                } else {
                                    let associated = b.load_typed(
                                        field_ptr,
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                    store_scalar_pointer_slot_value(b, &tgt_info, associated);
                                }
                                return;
                            }
                            if field.pointer {
                                let slot_value_ty = match &field.type_info {
                                    crate::sema::symtab::TypeInfo::Derived(_) => {
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))
                                    }
                                    _ => IrType::Ptr(Box::new(field_storage_ir_type(
                                        field,
                                        ctx.type_layouts,
                                    ))),
                                };
                                let associated = b.load_typed(field_ptr, slot_value_ty);
                                store_scalar_pointer_slot_value(b, &tgt_info, associated);
                                return;
                            }
                            // Cast ptr<i8> → ptr<tgt_ty> via zero-offset GEP
                            // so the store type matches the pointer slot.
                            let zero = b.const_i64(0);
                            let typed_ptr = b.gep(field_ptr, vec![zero], tgt_info.ty.clone());
                            store_scalar_pointer_slot_value(b, &tgt_info, typed_ptr);
                            return;
                        }
                    }
                }
            }

            if matches!(value.node, Expr::FunctionCall { .. }) {
                let addr = super::expr::lower_expr_ctx_tl(b, ctx, value);
                if local_uses_array_descriptor(&tgt_info) && tgt_info.dims.is_empty() {
                    let type_tag = static_expr_type_tag_value(b, value, ctx.st, ctx.type_layouts);
                    let elem_size = expr_type_layout(value, None, ctx.st, ctx.type_layouts)
                        .map(|layout| b.const_i64(layout.size as i64));
                    let vtable = static_expr_vtable_value(b, value, ctx.st, ctx.type_layouts);
                    let tgt_desc = array_descriptor_addr(b, &tgt_info);
                    store_scalar_polymorphic_descriptor_view(
                        b, tgt_desc, addr, elem_size, type_tag, vtable,
                    );
                } else {
                    store_scalar_pointer_slot_value(b, &tgt_info, addr);
                }
                return;
            }

            let Expr::Name { name: src_name } = &value.node else {
                return;
            };
            let src_key = src_name.to_lowercase();
            let Some(src_info) = ctx.locals.get(&src_key).cloned() else {
                if let Some(sym) = ctx.st.lookup_local_then_any(ctx.proc_scope_id, &src_key) {
                    if matches!(
                        sym.kind,
                        crate::sema::symtab::SymbolKind::Function
                            | crate::sema::symtab::SymbolKind::Subroutine
                    ) {
                        let (link_name, resolved_key) =
                            resolved_symbol_call_target(ctx.st, &src_key, src_name);
                        let lowered_name = if ctx.internal_funcs.contains_key(&resolved_key)
                            || ctx.internal_funcs.contains_key(&src_key)
                        {
                            lowered_procedure_symbol_name(
                                resolved_key.as_str(),
                                None,
                                Some(b.func().name.as_str()),
                                None,
                                true,
                                ctx.internal_funcs,
                            )
                        } else {
                            link_name
                        };
                        let addr = b.global_addr(
                            &lowered_name,
                            procedure_pointer_symbol_addr_elem_type(&tgt_info),
                        );
                        store_scalar_pointer_slot_value(b, &tgt_info, addr);
                    }
                }
                return;
            };

            // Array pointer path: materialise a descriptor from the
            // target and memcpy 392 bytes into the pointer's slot.
            // Both explicit-shape stack arrays and descriptor-backed
            // allocatables are supported via array_data_ptr_for_call.
            let target_is_array =
                !src_info.dims.is_empty() || src_info.allocatable || src_info.descriptor_arg;
            if target_is_array {
                let src_desc = if local_uses_array_descriptor(&src_info) {
                    array_descriptor_addr(b, &src_info)
                } else {
                    materialize_array_descriptor_for_info(b, &src_info)
                };
                if local_uses_array_descriptor(&tgt_info) {
                    let tgt_desc = array_descriptor_addr(b, &tgt_info);
                    let size = b.const_i64(392);
                    b.call(
                        FuncRef::External("memcpy".into()),
                        vec![tgt_desc, src_desc, size],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                } else if src_info.dims.is_empty() {
                    let src_base = if local_uses_array_descriptor(&src_info) {
                        scalar_descriptor_base_addr_raw(b, &src_info)
                    } else {
                        array_base_addr(b, &src_info)
                    };
                    store_scalar_pointer_slot_value(b, &tgt_info, src_base);
                }
                return;
            }

            // Scalar / derived-type pointer path.
            let addr = if src_info.is_pointer {
                // Copy the current association of another pointer
                // (pointer-to-pointer, including derived-type pointer
                // chains).  For scalar pointers (ty = i32) the stored
                // value is Ptr<i32>; for DT pointers (ty = Ptr<i8>)
                // the stored value is already Ptr<i8> — wrapping
                // again would produce Ptr<Ptr<i8>> and fail the
                // verifier.  Use ty directly when it's already a
                // pointer.
                let load_ty = if src_info.ty.is_ptr() {
                    src_info.ty.clone()
                } else {
                    IrType::Ptr(Box::new(src_info.ty.clone()))
                };
                if src_info.by_ref {
                    let caller_slot = b.load_typed(
                        src_info.addr,
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                    );
                    b.load_typed(caller_slot, load_ty)
                } else {
                    b.load_typed(src_info.addr, load_ty)
                }
            } else if src_info.by_ref {
                // Dummy TARGETs and procedure dummies are stored as
                // caller-provided addresses inside the local slot.
                // Pointer association must load through that slot so
                // `p => x` binds to the caller's storage/symbol rather
                // than this callee's alloca.
                let load_ty = if src_info.ty.is_ptr() {
                    src_info.ty.clone()
                } else {
                    IrType::Ptr(Box::new(src_info.ty.clone()))
                };
                b.load_typed(src_info.addr, load_ty)
            } else if src_info.derived_type.is_some() {
                // Derived-type TARGET.  src_info.addr is a
                // ptr<[i8 x size]>; the pointer slot expects ptr<i8>.
                // A zero-offset GEP with element type i8 produces
                // the element-pointer view and round-trips through
                // the verifier.
                let zero = b.const_i64(0);
                b.gep(src_info.addr, vec![zero], IrType::Int(IntWidth::I8))
            } else {
                // Plain TARGET or ordinary scalar local: the local
                // alloca address IS the associated target.
                src_info.addr
            };
            if local_uses_array_descriptor(&tgt_info) && tgt_info.dims.is_empty() {
                let type_tag = static_expr_type_tag_value(b, value, ctx.st, ctx.type_layouts);
                let elem_size = expr_type_layout(value, None, ctx.st, ctx.type_layouts)
                    .map(|layout| b.const_i64(layout.size as i64));
                let vtable = static_expr_vtable_value(b, value, ctx.st, ctx.type_layouts);
                let tgt_desc = array_descriptor_addr(b, &tgt_info);
                store_scalar_polymorphic_descriptor_view(
                    b, tgt_desc, addr, elem_size, type_tag, vtable,
                );
            } else {
                store_scalar_pointer_slot_value(b, &tgt_info, addr);
            }
        }

        Stmt::SelectRank {
            selector,
            assoc_name,
            guards,
            ..
        } => {
            // Read rank from selector's descriptor (offset 16, i32 field).
            // For non-descriptor-backed selectors, fall back to the static
            // declared rank.
            let bb_end = b.create_block("select_rank_end");
            let selector_info = associate_alias_local_info(b, ctx, selector);
            let runtime_rank: ValueId = if let Some(info) = selector_info.as_ref() {
                if local_uses_array_descriptor(info) {
                    let desc = array_descriptor_addr(b, info);
                    let rank32 = load_array_desc_i32_field(b, desc, 16);
                    b.int_extend(rank32, IntWidth::I64, true)
                } else {
                    b.const_i64(local_declared_rank(info) as i64)
                }
            } else {
                b.const_i64(0)
            };

            // Install `v` as an alias for the selector inside each guard.
            let saved_alias = assoc_name.as_ref().and_then(|name| {
                let key = name.to_lowercase();
                ctx.locals.remove(&key)
            });
            if let (Some(name), Some(info)) = (assoc_name.as_ref(), selector_info.as_ref()) {
                ctx.locals.insert(name.to_lowercase(), info.clone());
            }

            let default_body = guards.iter().find_map(|guard| {
                if let crate::ast::stmt::RankGuard::RankDefault { body } = guard {
                    Some(body)
                } else {
                    None
                }
            });

            for guard in guards {
                use crate::ast::stmt::RankGuard;
                match guard {
                    RankGuard::Rank { rank, body } => {
                        let want = b.const_i64(*rank);
                        let matches = b.icmp(CmpOp::Eq, runtime_rank, want);
                        let bb_match = b.create_block("rank_match");
                        let bb_next = b.create_block("rank_next");
                        b.cond_branch(matches, bb_match, vec![], bb_next, vec![]);
                        b.set_block(bb_match);
                        lower_stmts(b, ctx, body);
                        if b.func().block(b.current_block()).terminator.is_none() {
                            b.branch(bb_end, vec![]);
                        }
                        b.set_block(bb_next);
                    }
                    RankGuard::RankStar { .. } | RankGuard::RankDefault { .. } => {
                        // Defaults handled after specific ranks.
                    }
                }
            }
            if let Some(body) = default_body {
                lower_stmts(b, ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(bb_end, vec![]);
                }
            } else if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_end, vec![]);
            }
            b.set_block(bb_end);

            if let Some(name) = assoc_name.as_ref() {
                let key = name.to_lowercase();
                ctx.locals.remove(&key);
                if let Some(saved) = saved_alias {
                    ctx.locals.insert(key, saved);
                }
            }
        }

        _ => {} // remaining statements (FORALL, WHERE, etc.) deferred
    }
}
