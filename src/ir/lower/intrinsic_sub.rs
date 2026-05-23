//! Lowering of Fortran intrinsic subroutines (CALL system_clock,
//! CALL date_and_time, CALL c_f_pointer, ...).
//!
//! Extracted from `core.rs` in Sprint 11 Stage B.2. Pure mechanical
//! move — behavior unchanged. Helpers consulted via `core::*`.

use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;

use super::core::*;
use super::ctx::{CharKind, LowerCtx};
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

    /// Helper: adapt an intrinsic out-arg to a runtime ABI that writes
    /// through an i64 slot. Non-i64 destinations get a temporary i64
    /// alloca followed by an explicit writeback after the runtime call.
    fn nth_arg_i64_out(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
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
                    if dest_ty == IrType::Int(IntWidth::I64) {
                        return (dest_ptr, None);
                    }
                    let tmp_ptr = b.alloca(IrType::Int(IntWidth::I64));
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

    /// Helper: adapt an intrinsic out-arg to a runtime ABI that writes
    /// through an f64 slot. Non-f64 destinations get a temporary f64
    /// alloca followed by an explicit writeback after the runtime call.
    fn nth_arg_f64_out(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[Option<crate::ast::expr::Argument>],
        n: usize,
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
                    if dest_ty == IrType::Float(FloatWidth::F64) {
                        return (dest_ptr, None);
                    }
                    let tmp_ptr = b.alloca(IrType::Float(FloatWidth::F64));
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

    let arg_slots = reorder_args_by_keyword_slots(args, name, ctx.st);
    let args = arg_slots.as_slice();

    fn move_alloc_target(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        expr: &crate::ast::expr::SpannedExpr,
    ) -> Option<(ValueId, bool)> {
        match &expr.node {
            Expr::ParenExpr { inner } => move_alloc_target(b, ctx, inner),
            Expr::Name { name } => {
                let info = ctx.locals.get(&name.to_lowercase())?;
                if matches!(info.char_kind, CharKind::Deferred) {
                    Some((string_descriptor_addr(b, info), true))
                } else if local_uses_array_descriptor(info) {
                    Some((array_descriptor_addr(b, info), false))
                } else {
                    None
                }
            }
            Expr::ComponentAccess { .. } => {
                if let Some(info) =
                    component_array_local_info(b, &ctx.locals, expr, ctx.st, ctx.type_layouts)
                {
                    return Some((array_descriptor_addr(b, &info), false));
                }
                resolve_component_field_access(b, &ctx.locals, expr, ctx.st, ctx.type_layouts)
                    .and_then(|(field_ptr, field)| {
                        if matches!(field_char_kind(&field), CharKind::Deferred) && field.size == 32
                        {
                            Some((field_ptr, true))
                        } else if field.allocatable && field.size == 384 {
                            Some((field_ptr, false))
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
            let Some((from_desc, from_is_string)) =
                from_expr.and_then(|e| move_alloc_target(b, ctx, e))
            else {
                eprintln!(
                    "armfortas: error: MOVE_ALLOC source must be a descriptor-backed allocatable variable"
                );
                std::process::exit(1);
            };
            let Some((to_desc, to_is_string)) = to_expr.and_then(|e| move_alloc_target(b, ctx, e))
            else {
                eprintln!(
                    "armfortas: error: MOVE_ALLOC destination must be a descriptor-backed allocatable variable"
                );
                std::process::exit(1);
            };
            if from_is_string != to_is_string {
                eprintln!(
                    "armfortas: error: MOVE_ALLOC source and destination must use matching descriptor kinds"
                );
                std::process::exit(1);
            }
            let runtime = if from_is_string {
                "afs_move_alloc_string"
            } else {
                "afs_move_alloc"
            };
            b.call(
                FuncRef::External(runtime.into()),
                vec![from_desc, to_desc],
                IrType::Void,
            );
            true
        }
        "system_clock" => {
            // call system_clock(count, count_rate, count_max) — all optional
            let (count, count_writeback) = nth_arg_i64_out(b, ctx, args, 0);
            let (rate, rate_writeback) = nth_arg_i64_out(b, ctx, args, 1);
            let (max, max_writeback) = nth_arg_i64_out(b, ctx, args, 2);
            b.call(
                FuncRef::External("afs_system_clock".into()),
                vec![count, rate, max],
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
            // call date_and_time(date, time, zone, values) — all optional strings/array
            // Runtime: afs_date_and_time(date_buf, date_len, time_buf, time_len, zone_buf, zone_len, values)
            let (date_ptr, date_len) = nth_arg_str(b, ctx, args, 0);
            let (time_ptr, time_len) = nth_arg_str(b, ctx, args, 1);
            let (zone_ptr, zone_len) = nth_arg_str(b, ctx, args, 2);
            let values = nth_arg_ref(b, ctx, args, 3);
            b.call(
                FuncRef::External("afs_date_and_time".into()),
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
            let length = nth_arg_ref(b, ctx, args, 2);
            let status = nth_arg_ref(b, ctx, args, 3);
            b.call(
                FuncRef::External("afs_get_command_argument".into()),
                vec![number, val_ptr, val_len, length, status],
                IrType::Void,
            );
            true
        }
        "command_argument_count" => {
            // This is a function, not a subroutine — handled in lower_intrinsic.
            false
        }
        "get_command" => {
            // call get_command(command, length, status)
            let (cmd_ptr, cmd_len) = nth_arg_str(b, ctx, args, 0);
            let length = nth_arg_ref(b, ctx, args, 1);
            let status = nth_arg_ref(b, ctx, args, 2);
            b.call(
                FuncRef::External("afs_get_command".into()),
                vec![cmd_ptr, cmd_len, length, status],
                IrType::Void,
            );
            true
        }
        "get_environment_variable" => {
            // call get_environment_variable(name, value, length, status)
            // Runtime: afs_get_environment_variable(name, name_len, value, value_len, length, status)
            let (name_ptr, name_len) = nth_arg_str(b, ctx, args, 0);
            let (val_ptr, val_len) = nth_arg_str(b, ctx, args, 1);
            let length = nth_arg_ref(b, ctx, args, 2);
            let status = nth_arg_ref(b, ctx, args, 3);
            b.call(
                FuncRef::External("afs_get_environment_variable".into()),
                vec![name_ptr, name_len, val_ptr, val_len, length, status],
                IrType::Void,
            );
            true
        }
        "random_number" => {
            // F2018 §16.9.171: RANDOM_NUMBER(harvest) accepts both
            // scalar and array harvest. The scalar runtime fills only
            // one element; routing array actuals to it left N-1 slots
            // as stack garbage, which surfaced as denormal/NaN values
            // throughout stdlib examples (e.g. sparse_spmv: count() on
            // the resulting matrix returned 1 instead of m*n, malloc
            // sized COO%index(2,1), and the next assign ran past dim 0).
            let harvest = nth_arg_ref(b, ctx, args, 0);
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
                if let Some(expr) = harvest_expr {
                    if let Some((desc, _elem_ty)) = lower_array_expr_descriptor(
                        b,
                        &ctx.locals,
                        expr,
                        ctx.st,
                        Some(ctx.type_layouts),
                        Some(ctx.internal_funcs),
                        Some(ctx.contained_host_refs),
                        Some(ctx.descriptor_params),
                    ) {
                        let n = b.call(
                            FuncRef::External("afs_array_size".into()),
                            vec![desc],
                            IrType::Int(IntWidth::I64),
                        );
                        let runtime = if kind_is_f32 {
                            "afs_random_number_array_f32"
                        } else {
                            "afs_random_number_array_f64"
                        };
                        b.call(
                            FuncRef::External(runtime.into()),
                            vec![harvest, n],
                            IrType::Void,
                        );
                        return true;
                    }
                }
            }
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
            let exitstat = nth_arg_ref(b, ctx, args, 2);
            let cmdstat = nth_arg_ref(b, ctx, args, 3);
            b.call(
                FuncRef::External("afs_execute_command_line".into()),
                vec![cmd_ptr, cmd_len, wait, exitstat, cmdstat],
                IrType::Void,
            );
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

        // ---- iso_c_binding subroutines ----
        "c_f_pointer" => {
            // call c_f_pointer(cptr, fptr [, shape])
            //
            // Scalar pointers store the raw address directly into the
            // pointer slot. Array pointers are descriptor-backed in
            // armfortas, so we must populate the 384-byte descriptor
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
                        let sz384 = b.const_i64(384);
                        b.call(
                            FuncRef::External("memset".into()),
                            vec![target_addr, zero32, sz384],
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
                        let elem_size = b.const_i64(ir_scalar_byte_size(&elem_ty));
                        store_byte_aggregate_field(
                            b,
                            target_addr,
                            8,
                            IrType::Int(IntWidth::I64),
                            elem_size,
                        );

                        let shape_vals = c_f_pointer_shape_values(args);
                        let rank = shape_vals.map_or(0, |vals| vals.len());
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

                        if let Some(values) = shape_vals {
                            for (i, value) in values.iter().enumerate() {
                                let crate::ast::expr::AcValue::Expr(extent_expr) = value else {
                                    continue;
                                };
                                let raw_extent = super::expr::lower_expr_ctx(b, ctx, extent_expr);
                                let extent = match b.func().value_type(raw_extent) {
                                    Some(IrType::Int(IntWidth::I64)) => raw_extent,
                                    _ => b.int_extend(raw_extent, IntWidth::I64, true),
                                };
                                let base = 24 + (i as i64) * 24;
                                let one64 = b.const_i64(1);
                                store_byte_aggregate_field(
                                    b,
                                    target_addr,
                                    base,
                                    IrType::Int(IntWidth::I64),
                                    one64,
                                );
                                store_byte_aggregate_field(
                                    b,
                                    target_addr,
                                    base + 8,
                                    IrType::Int(IntWidth::I64),
                                    extent,
                                );
                                let stride64 = b.const_i64(1);
                                store_byte_aggregate_field(
                                    b,
                                    target_addr,
                                    base + 16,
                                    IrType::Int(IntWidth::I64),
                                    stride64,
                                );
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
