//! Lowering of Fortran intrinsic subroutines (CALL system_clock,
//! CALL date_and_time, CALL c_f_pointer, ...).
//!
//! Extracted from `core.rs` in Sprint 11 Stage B.2. Pure mechanical
//! move — behavior unchanged. Helpers consulted via `core::*`.

use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;

use super::core::*;
use super::ctx::{CharKind, LocalInfo, LowerCtx};
use super::helpers::coerce_to_type;
use crate::ast::expr::Expr;

/// Lower an intrinsic subroutine call (CALL system_clock, CALL date_and_time, etc.).
/// Returns true if the name was recognized and lowered, false otherwise.
pub(crate) fn lower_intrinsic_subroutine(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    name: &str,
    args: &[crate::ast::expr::Argument],
) -> bool {
    #[derive(Clone)]
    struct RuntimeOutWriteback {
        dest_ptr: ValueId,
        dest_ty: IrType,
        tmp_ptr: ValueId,
    }

    enum ProcPointerTarget {
        Local(LocalInfo),
        Component(ValueId),
    }

    /// Helper: get the nth positional arg as a by-ref pointer, or null if absent.
    fn nth_arg_ref(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
    ) -> ValueId {
        if let Some(Some(arg)) = args.get(n) {
            if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                return lower_arg_by_ref_ctx(b, ctx, e);
            }
        }
        b.const_i64(0) // null pointer for missing optional arg
    }

    fn nth_proc_pointer_target(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
    ) -> Option<ProcPointerTarget> {
        let arg = args.get(n)?.as_ref()?;
        let crate::ast::expr::SectionSubscript::Element(expr) = &arg.value else {
            return None;
        };
        match &expr.node {
            Expr::Name { name } => {
                let key = name.to_lowercase();
                let info = ctx.locals.get(&key)?;
                (info.is_pointer && info.dims.is_empty())
                    .then(|| ProcPointerTarget::Local(info.clone()))
            }
            Expr::ComponentAccess { .. } => {
                let (field_ptr, field) =
                    resolve_component_field_access(b, &ctx.locals, expr, ctx.st, ctx.type_layouts)?;
                (field.pointer && field.procedure_pointer)
                    .then_some(ProcPointerTarget::Component(field_ptr))
            }
            _ => None,
        }
    }

    /// Helper: get the nth positional arg as a by-value expression, or default.
    fn nth_arg_val(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
        default: i32,
    ) -> ValueId {
        if let Some(Some(arg)) = args.get(n) {
            if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                return super::expr::lower_expr_ctx(b, ctx, e);
            }
        }
        b.const_i32(default)
    }

    /// Helper: store an i32 runtime result into an out-arg pointer,
    /// coercing to the destination's pointee type (e.g. i32 tag into a
    /// default-logical or default-integer slot).
    fn store_i32_to_ref(b: &mut FuncBuilder, value: ValueId, dest: ValueId) {
        let pointee = match b.func().value_type(dest) {
            Some(IrType::Ptr(inner)) => (*inner).clone(),
            _ => IrType::Int(IntWidth::I32),
        };
        let coerced = coerce_to_type(b, value, &pointee);
        b.store(coerced, dest);
    }

    /// Helper: get the nth positional arg as a (ptr, len) string pair, or (null, 0).
    fn nth_arg_str(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
    ) -> (ValueId, ValueId) {
        if let Some(Some(arg)) = args.get(n) {
            if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                if expr_is_character_expr(b, &ctx.locals, e, ctx.st, Some(ctx.type_layouts)) {
                    return lower_string_expr_ctx(b, ctx, e);
                }
                // Otherwise pass as ref + zero length.
                let ptr = lower_arg_by_ref_ctx(b, ctx, e);
                let zero = b.const_i64(0);
                return (ptr, zero);
            }
        }
        let z = b.const_i64(0);
        (z, z)
    }

    /// Adapt an intrinsic out-arg to a fixed-width runtime ABI. A destination
    /// with a different kind gets a temporary of the ABI type followed by an
    /// explicit writeback after the runtime call.
    fn nth_arg_typed_out(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
        runtime_ty: IrType,
    ) -> (ValueId, Option<RuntimeOutWriteback>) {
        if let Some(Some(arg)) = args.get(n) {
            if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                let dest_ptr = lower_arg_by_ref_ctx(b, ctx, e);
                let semantic_dest_ty =
                    generic_actual_expr_type_info(e, &ctx.locals, ctx.st, Some(ctx.type_layouts))
                        .map(|ti| type_info_to_ir_type(&ti));
                let pointer_dest_ty = match b.func().value_type(dest_ptr) {
                    Some(IrType::Ptr(inner)) => Some((*inner).clone()),
                    _ => None,
                };
                if let Some(dest_ty) = semantic_dest_ty.or(pointer_dest_ty) {
                    if dest_ty == runtime_ty {
                        return (dest_ptr, None);
                    }
                    let tmp_ptr = b.alloca(runtime_ty);
                    return (
                        tmp_ptr,
                        Some(RuntimeOutWriteback {
                            dest_ptr,
                            dest_ty,
                            tmp_ptr,
                        }),
                    );
                }
                return (dest_ptr, None);
            }
        }
        (b.const_i64(0), None)
    }

    /// Adapt an intrinsic out-arg to a runtime ABI that writes through i32.
    fn nth_arg_i32_out(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
    ) -> (ValueId, Option<RuntimeOutWriteback>) {
        nth_arg_typed_out(b, ctx, args, n, IrType::Int(IntWidth::I32))
    }

    /// Adapt an intrinsic out-arg to a runtime ABI that writes through i64.
    fn nth_arg_i64_out(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
    ) -> (ValueId, Option<RuntimeOutWriteback>) {
        nth_arg_typed_out(b, ctx, args, n, IrType::Int(IntWidth::I64))
    }

    /// Adapt an intrinsic out-arg to a runtime ABI that writes through f64.
    fn nth_arg_f64_out(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
    ) -> (ValueId, Option<RuntimeOutWriteback>) {
        nth_arg_typed_out(b, ctx, args, n, IrType::Float(FloatWidth::F64))
    }

    fn emit_runtime_out_writeback(b: &mut FuncBuilder, writeback: RuntimeOutWriteback) {
        let raw = b.load(writeback.tmp_ptr);
        let coerced = coerce_to_type(b, raw, &writeback.dest_ty);
        b.store(coerced, writeback.dest_ptr);
    }

    let arg_slots = reorder_args_by_keyword_slots(args, name, ctx.st);
    let args = arg_slots.as_slice();

    struct MoveAllocTarget {
        desc: ValueId,
        is_string: bool,
        is_polymorphic: bool,
        is_unlimited_polymorphic: bool,
        derived_type: Option<String>,
    }

    fn move_alloc_target(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        expr: &crate::ast::expr::SpannedExpr,
    ) -> Option<MoveAllocTarget> {
        match &expr.node {
            Expr::ParenExpr { inner } => move_alloc_target(b, ctx, inner),
            Expr::Name { name } => {
                let info = ctx.locals.get(&name.to_lowercase())?;
                if matches!(info.char_kind, CharKind::Deferred) {
                    Some(MoveAllocTarget {
                        desc: string_descriptor_addr(b, info),
                        is_string: true,
                        is_polymorphic: false,
                        is_unlimited_polymorphic: false,
                        derived_type: None,
                    })
                } else if local_uses_array_descriptor(info) {
                    Some(MoveAllocTarget {
                        desc: array_descriptor_addr(b, info),
                        is_string: false,
                        is_polymorphic: info.is_class,
                        is_unlimited_polymorphic: info.is_class && info.derived_type.is_none(),
                        derived_type: info.derived_type.clone(),
                    })
                } else {
                    None
                }
            }
            Expr::ComponentAccess { .. } => {
                if let Some(info) =
                    component_array_local_info(b, &ctx.locals, expr, ctx.st, ctx.type_layouts)
                {
                    return Some(MoveAllocTarget {
                        desc: array_descriptor_addr(b, &info),
                        is_string: false,
                        is_polymorphic: info.is_class,
                        is_unlimited_polymorphic: info.is_class && info.derived_type.is_none(),
                        derived_type: info.derived_type,
                    });
                }
                resolve_component_field_access(b, &ctx.locals, expr, ctx.st, ctx.type_layouts)
                    .and_then(|(field_ptr, field)| {
                        if matches!(field_char_kind(&field), CharKind::Deferred) && field.size == 32
                        {
                            Some(MoveAllocTarget {
                                desc: field_ptr,
                                is_string: true,
                                is_polymorphic: false,
                                is_unlimited_polymorphic: false,
                                derived_type: None,
                            })
                        } else if field.allocatable && field.size == 392 {
                            Some(MoveAllocTarget {
                                desc: field_ptr,
                                is_string: false,
                                is_polymorphic: matches!(
                                    &field.type_info,
                                    crate::sema::symtab::TypeInfo::Class(_)
                                        | crate::sema::symtab::TypeInfo::ClassStar
                                        | crate::sema::symtab::TypeInfo::TypeStar
                                ),
                                is_unlimited_polymorphic: matches!(
                                    &field.type_info,
                                    crate::sema::symtab::TypeInfo::ClassStar
                                        | crate::sema::symtab::TypeInfo::TypeStar
                                ),
                                derived_type: field_derived_type_name(&field),
                            })
                        } else {
                            None
                        }
                    })
            }
            _ => None,
        }
    }

    fn nth_arg_expr(
        args: &[Option<crate::ast::expr::Argument>],
        idx: usize,
    ) -> Option<&crate::ast::expr::SpannedExpr> {
        let arg = args.get(idx)?.as_ref()?;
        if let crate::ast::expr::SectionSubscript::Element(expr) = &arg.value {
            Some(expr)
        } else {
            None
        }
    }

    /// The descriptor address and a clone of the LocalInfo for an
    /// allocatable rank-1 array passed by name. Used by TOKENIZE to
    /// allocate FIRST/LAST/TOKENS/SEPARATOR through the runtime.
    fn nth_arg_alloc_array(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
    ) -> Option<(ValueId, LocalInfo)> {
        let e = nth_arg_expr(args, n)?;
        if let Expr::Name { name } = &e.node {
            let info = ctx.locals.get(&name.to_lowercase())?.clone();
            if local_uses_array_descriptor(&info) {
                let desc = array_descriptor_addr(b, &info);
                return Some((desc, info));
            }
        }
        None
    }

    match name {
        "move_alloc" => {
            let from_expr = args.first().and_then(|arg| {
                let arg = arg.as_ref()?;
                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                    Some(e)
                } else {
                    None
                }
            });
            let to_expr = args.get(1).and_then(|arg| {
                let arg = arg.as_ref()?;
                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                    Some(e)
                } else {
                    None
                }
            });
            let Some(from) = from_expr.and_then(|e| move_alloc_target(b, ctx, e)) else {
                eprintln!(
                    "armfortas: error: MOVE_ALLOC source must be a descriptor-backed allocatable variable"
                );
                std::process::exit(1);
            };
            let Some(to) = to_expr.and_then(|e| move_alloc_target(b, ctx, e)) else {
                eprintln!(
                    "armfortas: error: MOVE_ALLOC destination must be a descriptor-backed allocatable variable"
                );
                std::process::exit(1);
            };
            if from.is_string != to.is_string {
                eprintln!(
                    "armfortas: error: MOVE_ALLOC source and destination must use matching descriptor kinds"
                );
                std::process::exit(1);
            }
            let stat_ptr = nth_arg_expr(args, 2).map(|expr| lower_arg_by_ref_ctx(b, ctx, expr));
            if to.is_polymorphic {
                require_context_free_dynamic_lifecycle(b, from.desc);
            }
            if to.is_unlimited_polymorphic {
                let stat_addr = b.alloca(IrType::Int(IntWidth::I32));
                let zero = b.const_i32(0);
                b.store(zero, stat_addr);
                let finalize = b.const_i32(1);
                release_unlimited_polymorphic_allocatable_descriptor(
                    b, to.desc, stat_addr, finalize,
                );
            } else if let Some(layout) = to
                .derived_type
                .as_deref()
                .and_then(|type_name| ctx.type_layouts.get(type_name))
            {
                if to.is_polymorphic {
                    require_context_free_dynamic_lifecycle(b, to.desc);
                }
                finalize_derived_descriptor_storage_if_allocated(
                    b,
                    ctx.st,
                    ctx.internal_funcs,
                    Some(ctx.contained_host_refs),
                    &ctx.locals,
                    to.desc,
                    layout,
                    ctx.type_layouts,
                );
                let stat_addr = b.alloca(IrType::Int(IntWidth::I32));
                let zero = b.const_i32(0);
                b.store(zero, stat_addr);
                deallocate_derived_descriptor_components(
                    b,
                    to.desc,
                    layout,
                    ctx.type_layouts,
                    stat_addr,
                );
            }
            let runtime = if from.is_string {
                "afs_move_alloc_string"
            } else {
                "afs_move_alloc"
            };
            b.call(
                FuncRef::External(runtime.into()),
                vec![from.desc, to.desc],
                IrType::Void,
            );
            if let Some(stat_ptr) = stat_ptr {
                let success = b.const_i32(0);
                store_i32_to_ref(b, success, stat_ptr);
            }
            true
        }
        "system_clock" => {
            // call system_clock(count, count_rate, count_max) — all optional.
            // The clock resolution keys off the smallest integer kind among
            // the present arguments (gfortran behavior): default-integer
            // callers get a millisecond clock that fits kind 4 instead of
            // truncated nanoseconds (COUNT_MAX used to read back as -1).
            let (count, count_writeback) = nth_arg_i64_out(b, ctx, args, 0);
            let (rate, rate_writeback) = nth_arg_i64_out(b, ctx, args, 1);
            let (max, max_writeback) = nth_arg_i64_out(b, ctx, args, 2);
            let mut kind: i64 = 16;
            for wb in [&count_writeback, &rate_writeback, &max_writeback]
                .into_iter()
                .flatten()
            {
                let k = match wb.dest_ty {
                    IrType::Int(IntWidth::I8) => 1,
                    IrType::Int(IntWidth::I16) => 2,
                    IrType::Int(IntWidth::I32) => 4,
                    IrType::Int(IntWidth::I64) => 8,
                    IrType::Int(IntWidth::I128) => 16,
                    _ => continue, // REAL count_rate doesn't key the clock
                };
                kind = kind.min(k);
            }
            let kind_arg = b.const_i32(if kind == 16 { 8 } else { kind as i32 });
            b.call(
                FuncRef::External("afs_system_clock_k".into()),
                vec![count, rate, max, kind_arg],
                IrType::Void,
            );
            for writeback in [count_writeback, rate_writeback, max_writeback]
                .into_iter()
                .flatten()
            {
                let raw = b.load(writeback.tmp_ptr);
                let coerced = coerce_to_type(b, raw, &writeback.dest_ty);
                b.store(coerced, writeback.dest_ptr);
            }
            true
        }
        "split" => {
            // CALL SPLIT(STRING, SET, POS [, BACK]). POS is INTENT(INOUT)
            // iteration state — copy-in the current value, let the
            // runtime update it through an i64 slot, copy-out.
            let (str_ptr, str_len) = nth_arg_str(b, ctx, args, 0);
            let (set_ptr, set_len) = nth_arg_str(b, ctx, args, 1);
            let (pos_slot, pos_wb) = nth_arg_i64_out(b, ctx, args, 2);
            if let Some(wb) = &pos_wb {
                let cur = b.load(wb.dest_ptr);
                let cur_i64 = coerce_to_type(b, cur, &IrType::Int(IntWidth::I64));
                b.store(cur_i64, pos_slot);
            }
            let back = nth_arg_val(b, ctx, args, 3, 0);
            b.call(
                FuncRef::External("afs_split".into()),
                vec![str_ptr, str_len, set_ptr, set_len, pos_slot, back],
                IrType::Void,
            );
            if let Some(wb) = pos_wb {
                let raw = b.load(wb.tmp_ptr);
                let coerced = coerce_to_type(b, raw, &wb.dest_ty);
                b.store(coerced, wb.dest_ptr);
            }
            true
        }
        "tokenize" => {
            // CALL TOKENIZE(STRING, SET, FIRST, LAST)         — Form 2
            // CALL TOKENIZE(STRING, SET, TOKENS [, SEPARATOR]) — Form 1
            // The form is decided by the third argument's type: a
            // character array is TOKENS (Form 1), an integer array is
            // FIRST (Form 2). The runtime allocates each output array.
            let (str_ptr, str_len) = nth_arg_str(b, ctx, args, 0);
            let (set_ptr, set_len) = nth_arg_str(b, ctx, args, 1);
            let tokens_is_char = nth_arg_expr(args, 2).is_some_and(|e| {
                expr_is_character_expr(b, &ctx.locals, e, ctx.st, Some(ctx.type_layouts))
            });
            if tokens_is_char {
                if let Some((tok_desc, _)) = nth_arg_alloc_array(b, ctx, args, 2) {
                    let sep_desc = nth_arg_alloc_array(b, ctx, args, 3)
                        .map(|(d, _)| d)
                        .unwrap_or_else(|| b.const_i64(0));
                    let char_kind = b.const_i64(1);
                    b.call(
                        FuncRef::External("afs_tokenize_tokens".into()),
                        vec![
                            str_ptr, str_len, set_ptr, set_len, tok_desc, sep_desc, char_kind,
                        ],
                        IrType::Void,
                    );
                }
            } else if let (Some((first_desc, first_info)), Some((last_desc, last_info))) = (
                nth_arg_alloc_array(b, ctx, args, 2),
                nth_arg_alloc_array(b, ctx, args, 3),
            ) {
                let first_kind_bytes = first_info
                    .ty
                    .int_width()
                    .map(|w| (w.bits() / 8) as i64)
                    .unwrap_or(4);
                let last_kind_bytes = last_info
                    .ty
                    .int_width()
                    .map(|w| (w.bits() / 8) as i64)
                    .unwrap_or(4);
                let first_kind = b.const_i64(first_kind_bytes);
                let last_kind = b.const_i64(last_kind_bytes);
                b.call(
                    FuncRef::External("afs_tokenize_positions_kinds".into()),
                    vec![
                        str_ptr, str_len, set_ptr, set_len, first_desc, last_desc, first_kind,
                        last_kind,
                    ],
                    IrType::Void,
                );
            }
            true
        }
        "cpu_time" => {
            let (time, writeback) = nth_arg_f64_out(b, ctx, args, 0);
            b.call(
                FuncRef::External("afs_cpu_time".into()),
                vec![time],
                IrType::Void,
            );
            if let Some(writeback) = writeback {
                let raw = b.load(writeback.tmp_ptr);
                let coerced = coerce_to_type(b, raw, &writeback.dest_ty);
                b.store(coerced, writeback.dest_ptr);
            }
            true
        }
        "date_and_time" => {
            // DATE_AND_TIME(date, time, zone, values): strings are optional,
            // while a present VALUES actual is always passed as an array
            // descriptor so integer kind and section stride remain explicit.
            let (date_ptr, date_len) = nth_arg_str(b, ctx, args, 0);
            let (time_ptr, time_len) = nth_arg_str(b, ctx, args, 1);
            let (zone_ptr, zone_len) = nth_arg_str(b, ctx, args, 2);
            let values = if let Some(expr) = nth_arg_expr(args, 3) {
                let Some((descriptor, elem_ty)) = lower_array_expr_descriptor(
                    b,
                    &ctx.locals,
                    expr,
                    ctx.st,
                    Some(ctx.type_layouts),
                    Some(ctx.internal_funcs),
                    Some(ctx.contained_host_refs),
                    Some(ctx.descriptor_params),
                ) else {
                    eprintln!(
                        "armfortas: error: DATE_AND_TIME VALUES must be a rank-one INTEGER array"
                    );
                    std::process::exit(1);
                };
                if !matches!(elem_ty, IrType::Int(_)) {
                    eprintln!(
                        "armfortas: error: DATE_AND_TIME VALUES must be a rank-one INTEGER array"
                    );
                    std::process::exit(1);
                }
                descriptor
            } else {
                b.const_i64(0)
            };
            b.call(
                FuncRef::External("afs_date_and_time_desc".into()),
                vec![
                    date_ptr, date_len, time_ptr, time_len, zone_ptr, zone_len, values,
                ],
                IrType::Void,
            );
            true
        }
        "get_command_argument" => {
            // call get_command_argument(number, value, length, status)
            // Runtime: afs_get_command_argument(number, value, value_len, length, status)
            let number = nth_arg_val(b, ctx, args, 0, 0);
            let (val_ptr, val_len) = nth_arg_str(b, ctx, args, 1);
            let (length, length_writeback) = nth_arg_i32_out(b, ctx, args, 2);
            let (status, status_writeback) = nth_arg_i32_out(b, ctx, args, 3);
            b.call(
                FuncRef::External("afs_get_command_argument".into()),
                vec![number, val_ptr, val_len, length, status],
                IrType::Void,
            );
            for writeback in [length_writeback, status_writeback].into_iter().flatten() {
                emit_runtime_out_writeback(b, writeback);
            }
            true
        }
        "command_argument_count" => {
            // This is a function, not a subroutine — handled in lower_intrinsic.
            false
        }
        "get_command" => {
            // call get_command(command, length, status)
            let (cmd_ptr, cmd_len) = nth_arg_str(b, ctx, args, 0);
            let (length, length_writeback) = nth_arg_i32_out(b, ctx, args, 1);
            let (status, status_writeback) = nth_arg_i32_out(b, ctx, args, 2);
            b.call(
                FuncRef::External("afs_get_command".into()),
                vec![cmd_ptr, cmd_len, length, status],
                IrType::Void,
            );
            for writeback in [length_writeback, status_writeback].into_iter().flatten() {
                emit_runtime_out_writeback(b, writeback);
            }
            true
        }
        "get_environment_variable" => {
            // call get_environment_variable(name, value, length, status, trim_name)
            // Runtime: afs_get_environment_variable_trim(
            //     name, name_len, value, value_len, length, status, trim_name)
            let (name_ptr, name_len) = nth_arg_str(b, ctx, args, 0);
            let (val_ptr, val_len) = nth_arg_str(b, ctx, args, 1);
            let (length, length_writeback) = nth_arg_i32_out(b, ctx, args, 2);
            let (status, status_writeback) = nth_arg_i32_out(b, ctx, args, 3);
            let trim_name = nth_arg_val(b, ctx, args, 4, 1);
            let trim_name = coerce_to_type(b, trim_name, &IrType::Int(IntWidth::I32));
            b.call(
                FuncRef::External("afs_get_environment_variable_trim".into()),
                vec![
                    name_ptr, name_len, val_ptr, val_len, length, status, trim_name,
                ],
                IrType::Void,
            );
            for writeback in [length_writeback, status_writeback].into_iter().flatten() {
                emit_runtime_out_writeback(b, writeback);
            }
            true
        }
        "random_number" => {
            // F2018 §16.9.171: an array HARVEST is described by its
            // rank, extents, and signed memory strides. Passing only
            // its first address plus SIZE makes every section look
            // contiguous and defines storage outside the actual.
            let harvest_expr = nth_arg_expr(args, 0);
            let kind_is_f32 = harvest_expr
                .and_then(|expr| {
                    generic_actual_expr_type_info(expr, &ctx.locals, ctx.st, Some(ctx.type_layouts))
                })
                .map(|ty| {
                    matches!(
                        ty,
                        crate::sema::symtab::TypeInfo::Real { kind: Some(k) } if k <= 4
                    )
                })
                .unwrap_or(false);
            let is_array = harvest_expr
                .map(|e| expr_returns_array(e, &ctx.locals, ctx.st))
                .unwrap_or(false);
            if is_array {
                let Some(expr) = harvest_expr else {
                    unreachable!("array classification requires a present RANDOM_NUMBER HARVEST");
                };
                let Some((descriptor, elem_ty)) = lower_array_expr_descriptor(
                    b,
                    &ctx.locals,
                    expr,
                    ctx.st,
                    Some(ctx.type_layouts),
                    Some(ctx.internal_funcs),
                    Some(ctx.contained_host_refs),
                    Some(ctx.descriptor_params),
                ) else {
                    eprintln!(
                        "armfortas: error: RANDOM_NUMBER HARVEST array requires a descriptor"
                    );
                    std::process::exit(1);
                };
                if !matches!(elem_ty, IrType::Float(_)) {
                    eprintln!("armfortas: error: RANDOM_NUMBER HARVEST must be REAL");
                    std::process::exit(1);
                }
                b.call(
                    FuncRef::External("afs_random_number_array_desc".into()),
                    vec![descriptor],
                    IrType::Void,
                );
                return true;
            }
            let harvest = nth_arg_ref(b, ctx, args, 0);
            let runtime = if kind_is_f32 {
                "afs_random_number_f32"
            } else {
                "afs_random_number_f64"
            };
            b.call(
                FuncRef::External(runtime.into()),
                vec![harvest],
                IrType::Void,
            );
            true
        }
        "random_seed" => {
            let has_size = nth_arg_expr(args, 0).is_some();
            let has_put = nth_arg_expr(args, 1).is_some();
            let has_get = nth_arg_expr(args, 2).is_some();

            if !has_size && !has_put && !has_get {
                b.call(
                    FuncRef::External("afs_random_seed_init".into()),
                    vec![],
                    IrType::Void,
                );
                return true;
            }

            if has_size {
                let (size, writeback) = nth_arg_i64_out(b, ctx, args, 0);
                b.call(
                    FuncRef::External("afs_random_seed_size".into()),
                    vec![size],
                    IrType::Void,
                );
                if let Some(writeback) = writeback {
                    let raw = b.load(writeback.tmp_ptr);
                    let coerced = coerce_to_type(b, raw, &writeback.dest_ty);
                    b.store(coerced, writeback.dest_ptr);
                }
            }

            if let Some(put_expr) = nth_arg_expr(args, 1) {
                if let Some((desc, _elem_ty)) = lower_array_expr_descriptor(
                    b,
                    &ctx.locals,
                    put_expr,
                    ctx.st,
                    Some(ctx.type_layouts),
                    Some(ctx.internal_funcs),
                    Some(ctx.contained_host_refs),
                    Some(ctx.descriptor_params),
                ) {
                    b.call(
                        FuncRef::External("afs_random_seed_put".into()),
                        vec![desc],
                        IrType::Void,
                    );
                } else {
                    let seed = super::expr::lower_expr_ctx(b, ctx, put_expr);
                    let widened = b.int_extend(seed, IntWidth::I64, true);
                    b.call(
                        FuncRef::External("afs_random_seed".into()),
                        vec![widened],
                        IrType::Void,
                    );
                }
            }

            if let Some(get_expr) = nth_arg_expr(args, 2) {
                if let Some((desc, _elem_ty)) = lower_array_expr_descriptor(
                    b,
                    &ctx.locals,
                    get_expr,
                    ctx.st,
                    Some(ctx.type_layouts),
                    Some(ctx.internal_funcs),
                    Some(ctx.contained_host_refs),
                    Some(ctx.descriptor_params),
                ) {
                    b.call(
                        FuncRef::External("afs_random_seed_get".into()),
                        vec![desc],
                        IrType::Void,
                    );
                }
            }
            true
        }
        "execute_command_line" => {
            let (cmd_ptr, cmd_len) = nth_arg_str(b, ctx, args, 0);
            let wait = nth_arg_val(b, ctx, args, 1, 1);
            let (exitstat, exitstat_writeback) = nth_arg_i32_out(b, ctx, args, 2);
            let (cmdstat, cmdstat_writeback) = nth_arg_i32_out(b, ctx, args, 3);
            b.call(
                FuncRef::External("afs_execute_command_line".into()),
                vec![cmd_ptr, cmd_len, wait, exitstat, cmdstat],
                IrType::Void,
            );
            for writeback in [exitstat_writeback, cmdstat_writeback]
                .into_iter()
                .flatten()
            {
                emit_runtime_out_writeback(b, writeback);
            }
            true
        }
        "flush" => {
            let unit_raw = nth_arg_val(b, ctx, args, 0, 6);
            let unit = coerce_to_type(b, unit_raw, &IrType::Int(IntWidth::I32));
            let null = b.const_i64(0);
            b.call(
                FuncRef::External("afs_flush".into()),
                vec![unit, null],
                IrType::Void,
            );
            true
        }

        // ---- IEEE FP-environment subroutines ----
        // These touch the hardware FP control/status word, so the call
        // is a barrier the optimizer must not move FP ops across; the
        // runtime call is conservatively a barrier already (l09).
        "ieee_set_rounding_mode" => {
            let mode = nth_arg_val(b, ctx, args, 0, 0);
            let mode = coerce_to_type(b, mode, &IrType::Int(IntWidth::I32));
            b.call(
                FuncRef::External("afs_ieee_set_rounding".into()),
                vec![mode],
                IrType::Void,
            );
            true
        }
        "ieee_get_rounding_mode" => {
            let dest = nth_arg_ref(b, ctx, args, 0);
            let r = b.call(
                FuncRef::External("afs_ieee_get_rounding".into()),
                vec![],
                IrType::Int(IntWidth::I32),
            );
            store_i32_to_ref(b, r, dest);
            true
        }
        "ieee_set_flag" => {
            let flag = nth_arg_val(b, ctx, args, 0, 0);
            let flag = coerce_to_type(b, flag, &IrType::Int(IntWidth::I32));
            let val = nth_arg_val(b, ctx, args, 1, 0);
            let val = coerce_to_type(b, val, &IrType::Int(IntWidth::I32));
            b.call(
                FuncRef::External("afs_ieee_set_flag".into()),
                vec![flag, val],
                IrType::Void,
            );
            true
        }
        "ieee_get_flag" => {
            let flag = nth_arg_val(b, ctx, args, 0, 0);
            let flag = coerce_to_type(b, flag, &IrType::Int(IntWidth::I32));
            let dest = nth_arg_ref(b, ctx, args, 1);
            let r = b.call(
                FuncRef::External("afs_ieee_test_flag".into()),
                vec![flag],
                IrType::Int(IntWidth::I32),
            );
            store_i32_to_ref(b, r, dest);
            true
        }
        "ieee_get_status" => {
            let dest = nth_arg_ref(b, ctx, args, 0);
            b.call(
                FuncRef::External("afs_ieee_get_status".into()),
                vec![dest],
                IrType::Void,
            );
            true
        }
        "ieee_set_status" => {
            let src = nth_arg_ref(b, ctx, args, 0);
            b.call(
                FuncRef::External("afs_ieee_set_status".into()),
                vec![src],
                IrType::Void,
            );
            true
        }
        "ieee_get_halting_mode" => {
            // Halting (trap) is unsupported (ieee_support_halting=false);
            // report it disabled rather than lie.
            let dest = nth_arg_ref(b, ctx, args, 1);
            let zero = b.const_i32(0);
            store_i32_to_ref(b, zero, dest);
            true
        }
        "ieee_set_halting_mode" => {
            // No-op: traps are unsupported. A conformant program checks
            // ieee_support_halting first and never requests enabling.
            true
        }

        // ---- iso_c_binding subroutines ----
        "c_f_pointer" => {
            // call c_f_pointer(cptr, fptr [, shape])
            //
            // Scalar pointers store the raw address directly into the
            // pointer slot. Array pointers are descriptor-backed in
            // armfortas, so we must populate the 392-byte descriptor
            // with base_addr/elem_size/rank/bounds instead of
            // treating the second argument like a plain Ptr<T>.
            let raw_cptr = nth_arg_val(b, ctx, args, 0, 0);
            let cptr = match b.func().value_type(raw_cptr) {
                Some(IrType::Int(IntWidth::I64)) => raw_cptr,
                _ => b.int_extend(raw_cptr, IntWidth::I64, false),
            };

            let target_expr = args.get(1).and_then(|arg| {
                let arg = arg.as_ref()?;
                if let crate::ast::expr::SectionSubscript::Element(expr) = &arg.value {
                    Some(expr)
                } else {
                    None
                }
            });
            if let Some(expr) = target_expr {
                if let Some((target_addr, elem_ty, descriptor_backed)) =
                    c_f_pointer_target(b, ctx, expr)
                {
                    if descriptor_backed {
                        let zero32 = b.const_i32(0);
                        let descriptor_bytes = b.const_i64(392);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![target_addr, zero32, descriptor_bytes],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        );

                        let base_ptr = b.int_to_ptr(cptr, elem_ty.clone());
                        store_byte_aggregate_field(
                            b,
                            target_addr,
                            0,
                            IrType::Ptr(Box::new(elem_ty.clone())),
                            base_ptr,
                        );
                        let elem_size = b.const_i64(ir_scalar_byte_size(&elem_ty, ctx.layout));
                        store_byte_aggregate_field(
                            b,
                            target_addr,
                            8,
                            IrType::Int(IntWidth::I64),
                            elem_size,
                        );

                        // SHAPE and optional LOWER (F2023). Both may be inline
                        // constructors or runtime integer arrays. Rank comes
                        // from a literal shape's length, else from the FPTR's
                        // declared rank. Each dim stores {lower, upper, 1}
                        // where upper = lower + extent - 1 (lower defaults to
                        // 1 — preserving the pre-LOWER behavior exactly).
                        let shape_src = c_f_pointer_dim_arg(b, ctx, args, 2);
                        let lower_src = c_f_pointer_dim_arg(b, ctx, args, 3);
                        let fptr_rank = match &expr.node {
                            Expr::Name { name } => {
                                ctx.locals.get(&name.to_lowercase()).map(|i| i.dims.len())
                            }
                            _ => None,
                        };
                        let rank = match &shape_src {
                            Some(CfpDimSource::Literal(vals)) => vals.len(),
                            Some(CfpDimSource::Runtime(..)) => {
                                // A SHAPE argument means FPTR is an array, so its
                                // rank equals the shape length (== FPTR's declared
                                // rank). Defaulting to 0 would build a scalar
                                // descriptor and miscompile every array access
                                // (audit T3); fail loudly if neither source knows.
                                c_f_pointer_shape_static_rank(ctx, args)
                                    .or(fptr_rank)
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "c_f_pointer: cannot determine result rank for a \
                                             runtime SHAPE with an FPTR of unknown declared rank"
                                        )
                                    })
                            }
                            None => 0,
                        };
                        let rank_val = b.const_i32(rank as i32);
                        store_byte_aggregate_field(
                            b,
                            target_addr,
                            16,
                            IrType::Int(IntWidth::I32),
                            rank_val,
                        );

                        let null_cptr = b.const_i64(0);
                        let is_associated = b.icmp(CmpOp::Ne, cptr, null_cptr);
                        let assoc_flag = b.const_i32(2);
                        let disassoc_flag = b.const_i32(0);
                        let flags = b.select(is_associated, assoc_flag, disassoc_flag);
                        store_byte_aggregate_field(
                            b,
                            target_addr,
                            20,
                            IrType::Int(IntWidth::I32),
                            flags,
                        );

                        if let Some(shape_src) = &shape_src {
                            // Column-major strides accumulate across dims:
                            // stride[0] = 1, stride[i] = stride[i-1] *
                            // extent[i-1]. The pre-LOWER code stored 1 for
                            // every dim, which only happened to work for
                            // rank-1 pointers; rank>1 element access was wrong.
                            let mut running_stride = b.const_i64(1);
                            for i in 0..rank {
                                let Some(extent) = c_f_pointer_dim_value(b, ctx, shape_src, i)
                                else {
                                    continue;
                                };
                                let lower = lower_src
                                    .as_ref()
                                    .and_then(|ls| c_f_pointer_dim_value(b, ctx, ls, i))
                                    .unwrap_or_else(|| b.const_i64(1));
                                let sum = b.iadd(lower, extent);
                                let one64 = b.const_i64(1);
                                let upper = b.isub(sum, one64);
                                let base = 24 + (i as i64) * 24;
                                store_byte_aggregate_field(
                                    b,
                                    target_addr,
                                    base,
                                    IrType::Int(IntWidth::I64),
                                    lower,
                                );
                                store_byte_aggregate_field(
                                    b,
                                    target_addr,
                                    base + 8,
                                    IrType::Int(IntWidth::I64),
                                    upper,
                                );
                                store_byte_aggregate_field(
                                    b,
                                    target_addr,
                                    base + 16,
                                    IrType::Int(IntWidth::I64),
                                    running_stride,
                                );
                                running_stride = b.imul(running_stride, extent);
                            }
                        }
                        return true;
                    }

                    let ptr_val = b.int_to_ptr(cptr, elem_ty);
                    b.store(ptr_val, target_addr);
                    return true;
                }
            }

            let fptr = nth_arg_ref(b, ctx, args, 1);
            let inner_pointee = b
                .func()
                .value_type(fptr)
                .and_then(|ty| {
                    if let IrType::Ptr(inner) = ty {
                        if let IrType::Ptr(elem) = inner.as_ref() {
                            Some(elem.as_ref().clone())
                        } else {
                            Some(inner.as_ref().clone())
                        }
                    } else {
                        None
                    }
                })
                .unwrap_or(IrType::Int(IntWidth::I8));
            let ptr_val = b.int_to_ptr(cptr, inner_pointee);
            b.store(ptr_val, fptr);
            true
        }

        "c_f_procpointer" => {
            // call c_f_procpointer(cptr, fptr)
            //
            // Procedure pointers are represented as raw code pointers in
            // local storage, matching type(c_funptr)'s opaque i64 payload.
            // Associating one is therefore the scalar c_f_pointer case:
            // convert the incoming C_FUNPTR value to the FPTR slot's
            // pointee type and store it.
            let raw_cptr = nth_arg_val(b, ctx, args, 0, 0);
            let cptr = match b.func().value_type(raw_cptr) {
                Some(IrType::Int(IntWidth::I64)) => raw_cptr,
                _ => b.int_extend(raw_cptr, IntWidth::I64, false),
            };
            if let Some(target) = nth_proc_pointer_target(b, ctx, args, 1) {
                match target {
                    ProcPointerTarget::Local(info) => {
                        let elem_ty = procedure_pointer_symbol_addr_elem_type(&info);
                        let ptr_val = b.int_to_ptr(cptr, elem_ty);
                        store_scalar_pointer_slot_value(b, &info, ptr_val);
                    }
                    ProcPointerTarget::Component(field_ptr) => {
                        let ptr_val = b.int_to_ptr(cptr, IrType::Int(IntWidth::I8));
                        store_procedure_pointer_component_record(b, field_ptr, ptr_val, &[]);
                    }
                }
                return true;
            }

            let fptr = nth_arg_ref(b, ctx, args, 1);
            let inner_pointee = b
                .func()
                .value_type(fptr)
                .and_then(|ty| {
                    if let IrType::Ptr(inner) = ty {
                        if let IrType::Ptr(elem) = inner.as_ref() {
                            Some(elem.as_ref().clone())
                        } else {
                            Some(inner.as_ref().clone())
                        }
                    } else {
                        None
                    }
                })
                .unwrap_or(IrType::Int(IntWidth::I8));
            let ptr_val = b.int_to_ptr(cptr, inner_pointee);
            b.store(ptr_val, fptr);
            true
        }

        "c_f_strpointer" => {
            // F2023 18.2.3.5:
            //   CALL C_F_STRPOINTER(CSTRARRAY, FSTRPTR [, NCHARS])
            //   CALL C_F_STRPOINTER(CSTRPTR,  FSTRPTR,  NCHARS)
            // Associate a deferred-length c_char pointer with a C string. The
            // length is the longest NUL-free prefix bounded by NCHARS (if
            // present) or the source array's size. No copy: FSTRPTR aliases
            // the C bytes, so afs_c_f_strpointer marks the descriptor pointer
            // (not allocatable) and no free path will touch the memory.
            let to_i64 = |b: &mut FuncBuilder, v: ValueId| match b.func().value_type(v) {
                Some(IrType::Int(IntWidth::I64)) => v,
                _ => b.int_extend(v, IntWidth::I64, true),
            };
            let src_expr = nth_arg_expr(args, 0);
            let fptr_expr = nth_arg_expr(args, 1);
            let nchars_present = matches!(args.get(2), Some(Some(_)));

            let out_desc = fptr_expr.and_then(|e| match &e.node {
                Expr::Name { name } => ctx.locals.get(&name.to_lowercase()).and_then(|info| {
                    if matches!(info.char_kind, CharKind::Deferred) {
                        Some(string_descriptor_addr(b, info))
                    } else {
                        None
                    }
                }),
                _ => None,
            });
            // FSTRPTR shape and form constraints are diagnosed in sema; bail
            // quietly if we somehow reach lowering with an invalid call.
            let (Some(out_desc), Some(src_expr)) = (out_desc, src_expr) else {
                return true;
            };

            let src_is_char =
                expr_is_character_expr(b, &ctx.locals, src_expr, ctx.st, Some(ctx.type_layouts));

            let (data_ptr, max_len) = if src_is_char {
                // CSTRARRAY form: base address + (NCHARS or array size) bound.
                let Some((desc, _elem_ty)) = lower_array_expr_descriptor(
                    b,
                    &ctx.locals,
                    src_expr,
                    ctx.st,
                    Some(ctx.type_layouts),
                    Some(ctx.internal_funcs),
                    Some(ctx.contained_host_refs),
                    Some(ctx.descriptor_params),
                ) else {
                    return true;
                };
                let base = b.load_typed(desc, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                let bound = if nchars_present {
                    let n = nth_arg_val(b, ctx, args, 2, 0);
                    to_i64(b, n)
                } else {
                    b.call(
                        FuncRef::External("afs_array_size".into()),
                        vec![desc],
                        IrType::Int(IntWidth::I64),
                    )
                };
                (base, bound)
            } else {
                // CSTRPTR form: type(c_ptr) address by value. NCHARS is
                // required (sema); -1 is an unbounded-strlen safety fallback.
                let addr = nth_arg_val(b, ctx, args, 0, 0);
                let addr64 = to_i64(b, addr);
                let data = b.int_to_ptr(addr64, IrType::Int(IntWidth::I8));
                let bound = if nchars_present {
                    let n = nth_arg_val(b, ctx, args, 2, 0);
                    to_i64(b, n)
                } else {
                    b.const_i64(-1)
                };
                (data, bound)
            };

            b.call(
                FuncRef::External("afs_c_f_strpointer".into()),
                vec![data_ptr, max_len, out_desc],
                IrType::Void,
            );
            true
        }

        "mvbits" => {
            // F2018 §16.9.155: call mvbits(from, frompos, len, to, topos)
            // Copies len bits starting at bit `frompos` of `from` into
            // `to` starting at bit `topos`.  Other bits of `to` are
            // unchanged.  Both `from` and `to` must be the same integer
            // kind; we pick the destination's width as authoritative
            // since we have to write back through that pointer.
            let to_arg = match args.get(3) {
                Some(Some(arg)) => arg,
                _ => return true,
            };
            let crate::ast::expr::SectionSubscript::Element(to_expr) = &to_arg.value else {
                return true;
            };
            let to_ptr = lower_arg_by_ref_ctx(b, ctx, to_expr);
            let to_width_from_expr =
                operator_expr_type_info(to_expr, Some(&ctx.locals), ctx.st, Some(ctx.type_layouts))
                    .or_else(|| {
                        fortran_type_to_type_info(&crate::sema::types::expr_type(to_expr, ctx.st))
                    })
                    .and_then(|ti| match type_info_to_ir_type(&ti) {
                        IrType::Int(width) => Some(width),
                        _ => None,
                    });
            let to_width =
                to_width_from_expr.unwrap_or_else(|| match b.func().value_type(to_ptr) {
                    Some(IrType::Ptr(inner)) => match inner.as_ref() {
                        IrType::Int(w) => *w,
                        _ => IntWidth::I32,
                    },
                    _ => IntWidth::I32,
                });
            let from_val = nth_arg_val(b, ctx, args, 0, 0);
            let from = coerce_int_like_to_width(b, from_val, to_width);
            let frompos_val = nth_arg_val(b, ctx, args, 1, 0);
            let frompos = coerce_int_like_to_width(b, frompos_val, to_width);
            let len_val = nth_arg_val(b, ctx, args, 2, 0);
            let len = coerce_int_like_to_width(b, len_val, to_width);
            let topos_val = nth_arg_val(b, ctx, args, 4, 0);
            let topos = coerce_int_like_to_width(b, topos_val, to_width);

            let one = int_const_for_width(b, to_width, 1);
            // (1 << len) - 1
            let one_shl = b.shl(one, len);
            let one_again = int_const_for_width(b, to_width, 1);
            let len_mask = b.isub(one_shl, one_again);
            // extracted = (from >> frompos) & len_mask
            let shifted = b.lshr(from, frompos);
            let extracted = b.bit_and(shifted, len_mask);
            // shifted_dest = extracted << topos
            let shifted_dest = b.shl(extracted, topos);
            // dest_mask = len_mask << topos
            let dest_mask = b.shl(len_mask, topos);
            let inv_mask = b.bit_not(dest_mask);
            let to_loaded = b.load_typed(to_ptr, IrType::Int(to_width));
            let cleared = b.bit_and(to_loaded, inv_mask);
            let updated = b.bit_or(cleared, shifted_dest);
            b.store(updated, to_ptr);
            true
        }

        _ => false,
    }
}
