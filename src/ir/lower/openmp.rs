//! Lowering for executable OpenMP constructs.
//!
//! Semantic validation currently admits capture-free `PARALLEL` regions,
//! numeric/logical scalar data, and shared fixed-shape numeric/logical arrays.
//! Each region is outlined into the fixed callback shape owned by the
//! ARMFORTAS OpenMP ABI and synchronously invoked through the runtime. Shared
//! addresses and firstprivate snapshots live in a compiler-private environment
//! whose lifetime is bounded by the synchronous join; private objects live in
//! each callback invocation's stack frame.

use crate::ast::openmp::{OpenMpClause, OpenMpConstruct};
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::{FuncRef, Function, Param, ValueId};
use crate::ir::types::{IntWidth, IrType};

use super::core::{collect_format_labels, collect_label_blocks, ensure_termination};
use super::ctx::{LocalInfo, LowerCtx, ProcScopeGuard};
use super::helpers::coerce_to_type;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureKind {
    Shared,
    Private,
    FirstPrivate,
    InlineConstant,
}

#[derive(Clone)]
struct Capture {
    name: String,
    info: LocalInfo,
    kind: CaptureKind,
}

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
            OpenMpClause::Shared(_)
            | OpenMpClause::Private(_)
            | OpenMpClause::FirstPrivate(_)
            | OpenMpClause::Default(_) => {}
            _ => unreachable!("unsupported OpenMP PARALLEL clause passed semantic validation"),
        }
    }

    let private_names: std::collections::HashSet<_> = clauses
        .iter()
        .filter_map(|clause| match clause {
            OpenMpClause::Private(names) => Some(names.as_slice()),
            _ => None,
        })
        .flatten()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let firstprivate_names: std::collections::HashSet<_> = clauses
        .iter()
        .filter_map(|clause| match clause {
            OpenMpClause::FirstPrivate(names) => Some(names.as_slice()),
            _ => None,
        })
        .flatten()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let shared_names: std::collections::HashSet<_> = clauses
        .iter()
        .filter_map(|clause| match clause {
            OpenMpClause::Shared(names) => Some(names.as_slice()),
            _ => None,
        })
        .flatten()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let predetermined_private =
        crate::sema::validate::openmp::predetermined_private_names(ctx.st, body);
    let mut seen_captures = std::collections::HashSet::new();
    let captures: Vec<Capture> = crate::sema::validate::openmp::capture_references(ctx.st, body)
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
            let kind = if private_names.contains(&name) {
                CaptureKind::Private
            } else if firstprivate_names.contains(&name) {
                CaptureKind::FirstPrivate
            } else if info.inline_const.is_some() {
                CaptureKind::InlineConstant
            } else if shared_names.contains(&name) {
                CaptureKind::Shared
            } else if predetermined_private.contains(&name) {
                CaptureKind::Private
            } else {
                CaptureKind::Shared
            };
            Capture { name, info, kind }
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

fn materialize_shared_environment(b: &mut FuncBuilder<'_>, captures: &[Capture]) -> ValueId {
    let addressed_count = captures
        .iter()
        .filter(|capture| {
            matches!(
                capture.kind,
                CaptureKind::Shared | CaptureKind::FirstPrivate
            )
        })
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
    for capture in captures {
        if !matches!(
            capture.kind,
            CaptureKind::Shared | CaptureKind::FirstPrivate
        ) {
            continue;
        }
        let outside_address = if capture.info.by_ref {
            b.load(capture.info.addr)
        } else {
            capture.info.addr
        };
        let environment_address = if capture.kind == CaptureKind::FirstPrivate {
            // Snapshot FIRSTPRIVATE before any implicit task begins. Each
            // callback invocation copies from this stable value into its own
            // task-local stack slot.
            let snapshot = b.alloca(capture.info.ty.clone());
            let value = b.load_typed(outside_address, capture.info.ty.clone());
            b.store(value, snapshot);
            snapshot
        } else {
            outside_address
        };
        let raw_address = b.ptr_to_int(environment_address);
        let index = b.const_i64(slot_index);
        let slot = b.gep(environment, vec![index], IrType::Int(IntWidth::I64));
        b.store(raw_address, slot);
        slot_index += 1;
    }
    let raw_environment = b.ptr_to_int(environment);
    b.int_to_ptr(raw_environment, IrType::Int(IntWidth::I8))
}

fn install_shared_captures(b: &mut FuncBuilder<'_>, ctx: &mut LowerCtx<'_>, captures: &[Capture]) {
    let mut slot_index = 0i64;
    let environment_slots = if captures.iter().any(|capture| {
        matches!(
            capture.kind,
            CaptureKind::Shared | CaptureKind::FirstPrivate
        )
    }) {
        let raw_environment = b.ptr_to_int(ValueId(0));
        Some(b.int_to_ptr(raw_environment, IrType::Int(IntWidth::I64)))
    } else {
        None
    };

    for capture in captures {
        let mut local = capture.info.clone();
        match capture.kind {
            CaptureKind::InlineConstant => {
                // PARAMETER values already lower from `inline_const`; give the
                // outlined function its own sentinel so no parent SSA value
                // leaks across the function boundary.
                local.addr = b.alloca(capture.info.ty.clone());
            }
            CaptureKind::Private => {
                local.addr = b.alloca(capture.info.ty.clone());
                local.by_ref = false;
                local.inline_const = None;
            }
            CaptureKind::Shared | CaptureKind::FirstPrivate => {
                let index = b.const_i64(slot_index);
                let slot = b.gep(
                    environment_slots.expect("missing OpenMP environment slots"),
                    vec![index],
                    IrType::Int(IntWidth::I64),
                );
                let raw_address = b.load_typed(slot, IrType::Int(IntWidth::I64));
                let environment_address = b.int_to_ptr(raw_address, capture.info.ty.clone());
                if capture.kind == CaptureKind::FirstPrivate {
                    let private = b.alloca(capture.info.ty.clone());
                    let value = b.load_typed(environment_address, capture.info.ty.clone());
                    b.store(value, private);
                    local.addr = private;
                    local.inline_const = None;
                } else {
                    local.addr = environment_address;
                }
                local.by_ref = false;
                slot_index += 1;
            }
        }
        ctx.locals.insert(capture.name.clone(), local);
    }
}
