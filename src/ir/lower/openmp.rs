//! Lowering for executable OpenMP constructs.
//!
//! Semantic validation currently admits capture-free and scalar-shared
//! `PARALLEL` regions. Each region is outlined into the fixed callback shape
//! owned by the ARMFORTAS OpenMP ABI and synchronously invoked through the
//! runtime. Shared addresses live in a compiler-private environment whose
//! lifetime is bounded by the synchronous join.

use crate::ast::openmp::{OpenMpClause, OpenMpConstruct};
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::{FuncRef, Function, Param, ValueId};
use crate::ir::types::{IntWidth, IrType};

use super::core::{collect_format_labels, collect_label_blocks, ensure_termination};
use super::ctx::{LocalInfo, LowerCtx, ProcScopeGuard};
use super::helpers::coerce_to_type;

pub(super) fn lower_construct(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    construct: &OpenMpConstruct,
) {
    let OpenMpConstruct::Parallel { clauses, body } = construct else {
        unreachable!(
            "unsupported OpenMP {} construct passed semantic validation",
            construct.name()
        );
    };

    let mut if_value = b.const_i32(1);
    let mut requested_threads = b.const_i32(0);
    for clause in clauses {
        match clause {
            OpenMpClause::If { condition, .. } => {
                let raw = super::expr::lower_expr_ctx(b, ctx, condition);
                let logical = coerce_to_type(b, raw, &IrType::Bool);
                if_value = coerce_to_type(b, logical, &IrType::Int(IntWidth::I32));
            }
            OpenMpClause::NumThreads(count) => {
                let raw = super::expr::lower_expr_ctx(b, ctx, count);
                requested_threads = coerce_to_type(b, raw, &IrType::Int(IntWidth::I32));
            }
            OpenMpClause::Shared(_) | OpenMpClause::Default(_) => {}
            _ => unreachable!("unsupported OpenMP PARALLEL clause passed semantic validation"),
        }
    }

    let mut seen_captures = std::collections::HashSet::new();
    let captures: Vec<(String, LocalInfo)> =
        crate::sema::validate::openmp::capture_references(ctx.st, body)
            .into_iter()
            .filter(|(name, _, role)| {
                (*role == crate::sema::validate::openmp::ReferenceRole::Value
                    || (*role == crate::sema::validate::openmp::ReferenceRole::Callable
                        && ctx
                            .st
                            .lookup_local_then_any(ctx.proc_scope_id, name)
                            .is_some_and(|symbol| {
                                matches!(
                                    symbol.kind,
                                    crate::sema::symtab::SymbolKind::Variable
                                        | crate::sema::symtab::SymbolKind::Parameter
                                        | crate::sema::symtab::SymbolKind::ProcedurePointer
                                )
                            })))
                    && seen_captures.insert(name.clone())
            })
            .map(|(name, _, _)| {
                let info = ctx.locals.get(&name).cloned().unwrap_or_else(|| {
                    panic!("validated OpenMP shared capture '{name}' has no lowering binding")
                });
                (name, info)
            })
            .collect();
    let environment = materialize_shared_environment(b, &captures);

    let callback_name = ctx.next_openmp_region_name();
    let local_modules = b.local_modules();
    let byte_ptr = IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)));
    let params = vec![
        Param {
            name: "environment".into(),
            ty: byte_ptr.clone(),
            id: ValueId(0),
            fortran_noalias: false,
        },
        Param {
            name: "thread_num".into(),
            ty: IrType::Int(IntWidth::I32),
            id: ValueId(1),
            fortran_noalias: false,
        },
        Param {
            name: "team_size".into(),
            ty: IrType::Int(IntWidth::I32),
            id: ValueId(2),
            fortran_noalias: false,
        },
    ];
    let mut callback = Function::new(callback_name.clone(), params, IrType::Void);
    callback.internal_only = true;

    let (outlined_globals, nested_callbacks) = {
        let mut outlined_ctx = LowerCtx::new(
            ctx.st,
            ctx.globals,
            ctx.type_layouts,
            ctx.alloc_return_funcs,
            ctx.optional_params,
            ctx.descriptor_params,
            ctx.internal_funcs,
            ctx.elemental_funcs,
            ctx.char_len_star_params,
            ctx.contained_host_refs,
            ctx.ambiguous_use_warnings.clone(),
            callback_name.clone(),
            ctx.layout,
            ctx.standard,
        );
        outlined_ctx.filtered_names = ctx.filtered_names.clone();
        outlined_ctx.proc_scope_id = ctx.proc_scope_id;

        {
            let mut outlined = FuncBuilder::new(&mut callback, ctx.layout);
            outlined.set_local_modules(local_modules);
            install_shared_captures(&mut outlined, &mut outlined_ctx, &captures);
            collect_label_blocks(&mut outlined, body, &mut outlined_ctx.label_blocks);
            collect_format_labels(body, &mut outlined_ctx.format_labels);
            let _scope = ProcScopeGuard::enter(outlined_ctx.proc_scope_id);
            super::stmt::lower_stmts(&mut outlined, &mut outlined_ctx, body);
            ensure_termination(&mut outlined, None);
        }

        (
            std::mem::take(&mut outlined_ctx.pending_globals),
            std::mem::take(&mut outlined_ctx.pending_functions),
        )
    };
    ctx.pending_globals.extend(outlined_globals);
    ctx.pending_functions.push(callback);
    ctx.pending_functions.extend(nested_callbacks);

    let entry = b.global_addr(&callback_name, IrType::Int(IntWidth::I8));
    let flags = b.const_i32(0);
    b.call(
        FuncRef::External("afs_omp_parallel_region".into()),
        vec![entry, environment, if_value, requested_threads, flags],
        IrType::Int(IntWidth::I32),
    );
}

fn materialize_shared_environment(
    b: &mut FuncBuilder<'_>,
    captures: &[(String, LocalInfo)],
) -> ValueId {
    let addressed_count = captures
        .iter()
        .filter(|(_, info)| info.inline_const.is_none())
        .count();
    if addressed_count == 0 {
        let null = b.const_i64(0);
        return b.int_to_ptr(null, IrType::Int(IntWidth::I8));
    }

    let environment = b.alloca(IrType::Array(
        Box::new(IrType::Int(IntWidth::I64)),
        addressed_count as u64,
    ));
    let mut slot_index = 0i64;
    for (_, info) in captures {
        if info.inline_const.is_some() {
            continue;
        }
        let address = if info.by_ref {
            b.load(info.addr)
        } else {
            info.addr
        };
        let raw_address = b.ptr_to_int(address);
        let index = b.const_i64(slot_index);
        let slot = b.gep(environment, vec![index], IrType::Int(IntWidth::I64));
        b.store(raw_address, slot);
        slot_index += 1;
    }
    let raw_environment = b.ptr_to_int(environment);
    b.int_to_ptr(raw_environment, IrType::Int(IntWidth::I8))
}

fn install_shared_captures(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    captures: &[(String, LocalInfo)],
) {
    let mut slot_index = 0i64;
    let environment_slots = if captures.iter().any(|(_, info)| info.inline_const.is_none()) {
        let raw_environment = b.ptr_to_int(ValueId(0));
        Some(b.int_to_ptr(raw_environment, IrType::Int(IntWidth::I64)))
    } else {
        None
    };

    for (name, outside) in captures {
        let mut shared = outside.clone();
        if outside.inline_const.is_some() {
            // PARAMETER values already lower from `inline_const`; give the
            // outlined function its own sentinel so no parent SSA value leaks
            // across the function boundary.
            shared.addr = b.alloca(outside.ty.clone());
        } else {
            let index = b.const_i64(slot_index);
            let slot = b.gep(
                environment_slots.expect("missing OpenMP environment slots"),
                vec![index],
                IrType::Int(IntWidth::I64),
            );
            let raw_address = b.load_typed(slot, IrType::Int(IntWidth::I64));
            shared.addr = b.int_to_ptr(raw_address, outside.ty.clone());
            shared.by_ref = false;
            slot_index += 1;
        }
        ctx.locals.insert(name.clone(), shared);
    }
}
