//! Lowering for executable OpenMP constructs.
//!
//! Semantic validation admits `PARALLEL`, canonical static worksharing `DO`,
//! and combined `PARALLEL DO` regions with static or dynamic scheduling over
//! the supported numeric/logical data environment. Parallel regions are
//! outlined into the fixed callback shape owned by the ARMFORTAS OpenMP ABI
//! and synchronously invoked through the runtime. Shared addresses, shared
//! owning/non-owning array descriptors, and firstprivate snapshots live in a
//! compiler-private environment whose lifetime is bounded by the synchronous
//! join. Private objects live in each callback invocation, using inline
//! storage below the compiler's stack threshold and owned descriptors above
//! it.

use crate::ast::expr::Expr;
use crate::ast::openmp::{
    OpenMpClause, OpenMpConstruct, OpenMpReductionOperator, OpenMpScheduleKind,
};
use crate::ast::stmt::{SpannedStmt, Stmt};
use crate::ast::Spanned;
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::{CmpOp, FuncRef, Function, Param, ValueId};
use crate::ir::types::{IntWidth, IrType};

use super::alloc::rewrite_heap_promoted_declared_bounds;
use super::core::{
    array_base_addr, array_descriptor_addr, collect_format_labels, collect_label_blocks,
    derived_layout_needs_component_deallocation, derived_storage_ir_type,
    emit_derived_private_allocation_copy, emit_derived_value_copy, emit_memcpy_bytes,
    ensure_termination, initialize_derived_storage, insert_implicit_dealloc, ir_scalar_byte_size,
    local_declared_rank, local_uses_array_descriptor, lower_do_loop,
    materialize_array_descriptor_for_info, materialize_array_section_source_descriptor, DoLoopBody,
    DoLoopFields,
};
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

struct MaterializedEnvironment {
    address: ValueId,
    cleanup_descriptors: Vec<ValueId>,
    derived_snapshots: std::collections::HashMap<String, LocalInfo>,
    worksharing_chunk_slot: Option<i64>,
}

#[derive(Default)]
struct InstalledCaptureCleanup {
    array_descriptors: Vec<ValueId>,
    private_derived_scalars: std::collections::HashMap<String, LocalInfo>,
}

struct ScalarReductionBinding {
    name: String,
    operator: OpenMpReductionOperator,
    shared: LocalInfo,
    private: LocalInfo,
}

const PRIVATE_ARRAY_STACK_THRESHOLD: i64 = 64 * 1024;

fn fixed_array_layout(info: &LocalInfo, layout: crate::target::TargetLayout) -> Option<(u64, i64)> {
    if info.dims.is_empty() {
        return None;
    }
    let elements = info.dims.iter().fold(1_u64, |count, (_, extent)| {
        count.saturating_mul((*extent).max(0) as u64)
    });
    let elem_bytes = ir_scalar_byte_size(&info.ty, layout).max(1);
    let bytes = elements
        .min(i64::MAX as u64)
        .saturating_mul(elem_bytes as u64)
        .min(i64::MAX as u64) as i64;
    Some((elements, bytes))
}

fn fixed_array_storage_type(info: &LocalInfo, layout: crate::target::TargetLayout) -> IrType {
    let (elements, _) = fixed_array_layout(info, layout)
        .expect("OpenMP fixed-array storage requested for a scalar capture");
    IrType::Array(Box::new(info.ty.clone()), elements.max(1))
}

fn fixed_array_uses_heap(info: &LocalInfo, layout: crate::target::TargetLayout) -> bool {
    fixed_array_layout(info, layout)
        .is_some_and(|(_, bytes)| bytes >= PRIVATE_ARRAY_STACK_THRESHOLD)
}

fn zero_array_descriptor(b: &mut FuncBuilder<'_>) -> ValueId {
    let descriptor = b.alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392));
    let zero = b.const_i32(0);
    let bytes = b.const_i64(392);
    b.call(
        FuncRef::External("memset".into()),
        vec![descriptor, zero, bytes],
        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
    );
    descriptor
}

fn is_allocatable_array(info: &LocalInfo) -> bool {
    info.allocatable && !info.is_pointer && local_declared_rank(info) > 0
}

fn is_pointer_array(info: &LocalInfo) -> bool {
    info.is_pointer && local_declared_rank(info) > 0
}

fn is_array(info: &LocalInfo) -> bool {
    local_declared_rank(info) > 0
}

fn capture_needs_environment(
    capture: &Capture,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> bool {
    matches!(
        capture.kind,
        CaptureKind::Shared | CaptureKind::FirstPrivate
    ) || (capture.kind == CaptureKind::Private && is_allocatable_array(&capture.info))
        || (capture.kind == CaptureKind::Private
            && capture
                .info
                .derived_type
                .as_deref()
                .and_then(|name| type_layouts.get(name))
                .is_some_and(|layout| {
                    derived_layout_needs_component_deallocation(layout, type_layouts)
                }))
}

fn snapshot_array_descriptor(b: &mut FuncBuilder<'_>, source: ValueId) -> ValueId {
    let snapshot = zero_array_descriptor(b);
    emit_memcpy_bytes(b, snapshot, source, 392);
    snapshot
}

fn allocate_pointer_private(
    b: &mut FuncBuilder<'_>,
    outside: &LocalInfo,
    source: Option<ValueId>,
) -> LocalInfo {
    debug_assert!(is_pointer_array(outside));
    let descriptor = zero_array_descriptor(b);
    if let Some(source) = source {
        // FIRSTPRIVATE pointer initialization has pointer-assignment
        // semantics: copy association and bounds, never the target data.
        emit_memcpy_bytes(b, descriptor, source, 392);
    }
    let mut private = outside.clone();
    private.addr = descriptor;
    private.by_ref = false;
    private.allocatable = true;
    private.descriptor_arg = false;
    private.inline_const = None;
    private.is_pointer = true;
    private.runtime_dim_upper.fill(None);
    private.last_dim_assumed_size = false;
    private
}

fn allocate_allocatable_private(
    b: &mut FuncBuilder<'_>,
    outside: &LocalInfo,
    source: ValueId,
    copy_values: bool,
) -> (LocalInfo, ValueId) {
    debug_assert!(is_allocatable_array(outside));
    let descriptor = zero_array_descriptor(b);
    let allocated = b.call(
        FuncRef::External("afs_array_allocated".into()),
        vec![source],
        IrType::Int(IntWidth::I32),
    );
    let zero = b.const_i32(0);
    let is_allocated = b.icmp(CmpOp::Ne, allocated, zero);
    let allocate_bb = b.create_block("omp_allocatable_private_allocate");
    let ready_bb = b.create_block("omp_allocatable_private_ready");
    b.cond_branch(is_allocated, allocate_bb, vec![], ready_bb, vec![]);

    b.set_block(allocate_bb);
    let null_stat = b.const_i64(0);
    b.call(
        FuncRef::External("afs_allocate_like".into()),
        vec![descriptor, source, null_stat],
        IrType::Void,
    );
    if copy_values {
        let null_stat = b.const_i64(0);
        b.call(
            FuncRef::External("afs_copy_array_data_no_realloc".into()),
            vec![descriptor, source, null_stat],
            IrType::Void,
        );
    }
    b.branch(ready_bb, vec![]);
    b.set_block(ready_bb);

    let mut private = outside.clone();
    private.addr = descriptor;
    private.by_ref = false;
    private.descriptor_arg = false;
    private.inline_const = None;
    private.is_pointer = false;
    private.runtime_dim_upper.fill(None);
    private.last_dim_assumed_size = false;
    (private, descriptor)
}

fn allocate_private_array(
    b: &mut FuncBuilder<'_>,
    outside: &LocalInfo,
    shape_descriptor: Option<ValueId>,
) -> (LocalInfo, Option<ValueId>) {
    let mut private = outside.clone();
    private.by_ref = false;
    private.allocatable = false;
    private.descriptor_arg = false;
    private.inline_const = None;
    private.is_pointer = false;
    private.runtime_dim_upper.clear();
    private.last_dim_assumed_size = false;

    if !fixed_array_uses_heap(outside, b.layout) {
        private.addr = b.alloca(fixed_array_storage_type(outside, b.layout));
        return (private, None);
    }

    let descriptor = zero_array_descriptor(b);
    if let Some(source) = shape_descriptor {
        let stat = b.alloca(IrType::Int(IntWidth::I32));
        let zero = b.const_i32(0);
        b.store(zero, stat);
        b.call(
            FuncRef::External("afs_allocate_like".into()),
            vec![descriptor, source, stat],
            IrType::Void,
        );
    } else {
        let (elements, _) = fixed_array_layout(outside, b.layout)
            .expect("OpenMP heap array allocation requested for a scalar capture");
        let elem_bytes = b.const_i64(ir_scalar_byte_size(&outside.ty, b.layout).max(1));
        let count = b.const_i64(elements.min(i64::MAX as u64) as i64);
        b.call(
            FuncRef::External("afs_allocate_1d".into()),
            vec![descriptor, elem_bytes, count],
            IrType::Void,
        );
        rewrite_heap_promoted_declared_bounds(b, descriptor, &outside.dims);
    }
    private.addr = descriptor;
    private.descriptor_arg = true;
    (private, Some(descriptor))
}

fn copy_array_data(
    b: &mut FuncBuilder<'_>,
    destination: &LocalInfo,
    source: ValueId,
    descriptor_copy: bool,
) {
    if descriptor_copy {
        let null_stat = b.const_i64(0);
        b.call(
            FuncRef::External("afs_copy_array_data_no_realloc".into()),
            vec![destination.addr, source, null_stat],
            IrType::Void,
        );
    } else {
        let (_, bytes) = fixed_array_layout(destination, b.layout)
            .expect("OpenMP array copy requested for a scalar capture");
        emit_memcpy_bytes(b, destination.addr, source, bytes);
    }
}

fn deallocate_array_descriptor(b: &mut FuncBuilder<'_>, descriptor: ValueId) {
    let allocated = b.call(
        FuncRef::External("afs_array_allocated".into()),
        vec![descriptor],
        IrType::Int(IntWidth::I32),
    );
    let zero = b.const_i32(0);
    let is_allocated = b.icmp(CmpOp::Ne, allocated, zero);
    let deallocate_bb = b.create_block("omp_array_private_deallocate");
    let done_bb = b.create_block("omp_array_private_deallocate_done");
    b.cond_branch(is_allocated, deallocate_bb, vec![], done_bb, vec![]);

    b.set_block(deallocate_bb);
    let stat = b.alloca(IrType::Int(IntWidth::I32));
    b.store(zero, stat);
    b.call(
        FuncRef::External("afs_deallocate_array".into()),
        vec![descriptor, stat],
        IrType::Void,
    );
    b.branch(done_bb, vec![]);
    b.set_block(done_bb);
}

fn shared_capture_uses_descriptor(info: &LocalInfo) -> bool {
    local_uses_array_descriptor(info)
        || info.last_dim_assumed_size
        || (!info.dims.is_empty() && info.runtime_dim_upper.iter().any(Option::is_some))
}

pub(super) fn lower_construct(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    construct: &OpenMpConstruct,
) {
    match construct {
        OpenMpConstruct::Parallel { clauses, body } => {
            lower_parallel_region(b, ctx, clauses, body, ParallelRegionBody::Statements(body));
        }
        OpenMpConstruct::Do { clauses, loop_stmt } => {
            lower_worksharing_loop(b, ctx, clauses, loop_stmt, false, None);
        }
        OpenMpConstruct::ParallelDo { clauses, loop_stmt } => {
            let capture_body = std::slice::from_ref(loop_stmt.as_ref());
            lower_parallel_region(
                b,
                ctx,
                clauses,
                capture_body,
                ParallelRegionBody::WorksharingDo { clauses, loop_stmt },
            );
        }
        OpenMpConstruct::Critical { name, body } => {
            lower_critical_region(b, ctx, name.as_deref(), body)
        }
    }
}

fn lower_critical_region(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    name: Option<&str>,
    body: &[SpannedStmt],
) {
    let name = name.unwrap_or_default().to_ascii_lowercase();
    let name_ptr = b.const_string(name.as_bytes());
    let name_len = b.const_i64(name.len() as i64);
    b.call(
        FuncRef::External("afs_omp_critical_enter".into()),
        vec![name_ptr, name_len],
        IrType::Int(IntWidth::I32),
    );
    super::stmt::lower_stmts(b, ctx, body);
    if b.func().block(b.current_block()).terminator.is_none() {
        b.call(
            FuncRef::External("afs_omp_critical_exit".into()),
            vec![name_ptr, name_len],
            IrType::Int(IntWidth::I32),
        );
    }
}

#[derive(Clone, Copy)]
enum ParallelRegionBody<'a> {
    Statements(&'a [SpannedStmt]),
    WorksharingDo {
        clauses: &'a [OpenMpClause],
        loop_stmt: &'a SpannedStmt,
    },
}

fn lower_parallel_region(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    clauses: &[OpenMpClause],
    capture_body: &[SpannedStmt],
    region_body: ParallelRegionBody<'_>,
) {
    let mut if_value = b.const_i32(1);
    let mut requested_threads = b.const_i32(0);
    let mut worksharing_chunk = None;
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
            OpenMpClause::Schedule {
                kind: OpenMpScheduleKind::Static | OpenMpScheduleKind::Dynamic,
                chunk_size: Some(chunk_size),
            } => {
                // A combined construct evaluates the schedule expression in
                // the encountering context. This preserves the original list
                // item when that variable is also privatized by the construct.
                let raw = super::expr::lower_expr_ctx(b, ctx, chunk_size);
                worksharing_chunk = Some(coerce_to_type(b, raw, &IrType::Int(IntWidth::I64)));
            }
            OpenMpClause::Shared(_)
            | OpenMpClause::Private(_)
            | OpenMpClause::FirstPrivate(_)
            | OpenMpClause::Default(_)
            | OpenMpClause::Schedule { .. }
            | OpenMpClause::Collapse(_)
            | OpenMpClause::Reduction { .. } => {}
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
        crate::sema::validate::openmp::predetermined_private_names(ctx.st, capture_body);
    let mut seen_captures = std::collections::HashSet::new();
    let mut captures: Vec<Capture> =
        crate::sema::validate::openmp::capture_references(ctx.st, capture_body)
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
    for clause in clauses {
        let OpenMpClause::Reduction { variables, .. } = clause else {
            continue;
        };
        for name in variables {
            let key = name.to_ascii_lowercase();
            if !seen_captures.insert(key.clone()) {
                continue;
            }
            let info = ctx.locals.get(&key).cloned().unwrap_or_else(|| {
                panic!("validated OpenMP reduction capture '{key}' has no lowering binding")
            });
            captures.push(Capture {
                name: key,
                info,
                kind: CaptureKind::Shared,
            });
        }
    }
    for capture in &mut captures {
        let Some(type_name) = capture.info.derived_type.as_deref() else {
            continue;
        };
        let canonical = ctx
            .proc_scope_id
            .and_then(|scope| ctx.type_layouts.canonical_name_for_scope(scope, type_name))
            .or_else(|| {
                ctx.type_layouts
                    .get(type_name)
                    .map(|layout| ctx.type_layouts.canonical_key_for_layout(layout))
            });
        if let Some(canonical) = canonical {
            capture.info.derived_type = Some(canonical);
        }
    }
    let environment =
        materialize_shared_environment(b, &captures, worksharing_chunk, ctx.type_layouts);
    let worksharing_chunk_slot = environment.worksharing_chunk_slot;

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
        outlined_ctx.openmp_team = Some((ValueId(1), ValueId(2)));

        {
            let mut outlined = FuncBuilder::new(&mut callback, ctx.layout);
            outlined.set_local_modules(local_modules);
            let cleanup = install_shared_captures(&mut outlined, &mut outlined_ctx, &captures);
            let parallel_reductions = match region_body {
                ParallelRegionBody::Statements(_) => {
                    prepare_scalar_reductions(&mut outlined, &mut outlined_ctx, clauses)
                }
                ParallelRegionBody::WorksharingDo { .. } => Vec::new(),
            };
            let outlined_worksharing_chunk = worksharing_chunk_slot.map(|slot_index| {
                let raw_environment = outlined.ptr_to_int(ValueId(0));
                let environment_slots =
                    outlined.int_to_ptr(raw_environment, IrType::Int(IntWidth::I64));
                let index = outlined.const_i64(slot_index);
                let slot = outlined.gep(environment_slots, vec![index], IrType::Int(IntWidth::I64));
                outlined.load_typed(slot, IrType::Int(IntWidth::I64))
            });
            collect_label_blocks(&mut outlined, capture_body, &mut outlined_ctx.label_blocks);
            collect_format_labels(capture_body, &mut outlined_ctx.format_labels);
            let _scope = ProcScopeGuard::enter(outlined_ctx.proc_scope_id);
            match region_body {
                ParallelRegionBody::Statements(body) => {
                    super::stmt::lower_stmts(&mut outlined, &mut outlined_ctx, body)
                }
                ParallelRegionBody::WorksharingDo { clauses, loop_stmt } => {
                    // The worksharing barrier and the immediately following
                    // parallel-region join are observably equivalent here:
                    // a combined construct has no intervening statements.
                    lower_worksharing_loop(
                        &mut outlined,
                        &mut outlined_ctx,
                        clauses,
                        loop_stmt,
                        true,
                        outlined_worksharing_chunk,
                    );
                }
            }
            if outlined
                .func()
                .block(outlined.current_block())
                .terminator
                .is_none()
            {
                finish_scalar_reductions(&mut outlined, &parallel_reductions, ValueId(1));
                if !cleanup.private_derived_scalars.is_empty() {
                    let closure_locals = outlined_ctx.locals.clone();
                    insert_implicit_dealloc(
                        &mut outlined,
                        &cleanup.private_derived_scalars,
                        &closure_locals,
                        outlined_ctx.type_layouts,
                        outlined_ctx.st,
                        outlined_ctx.internal_funcs,
                        Some(outlined_ctx.contained_host_refs),
                        None,
                        true,
                    );
                }
                for descriptor in cleanup.array_descriptors {
                    deallocate_array_descriptor(&mut outlined, descriptor);
                }
            }
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
        vec![
            entry,
            environment.address,
            if_value,
            requested_threads,
            flags,
        ],
        IrType::Int(IntWidth::I32),
    );
    if !environment.derived_snapshots.is_empty() {
        let closure_locals = ctx.locals.clone();
        insert_implicit_dealloc(
            b,
            &environment.derived_snapshots,
            &closure_locals,
            ctx.type_layouts,
            ctx.st,
            ctx.internal_funcs,
            Some(ctx.contained_host_refs),
            None,
            false,
        );
    }
    for descriptor in environment.cleanup_descriptors {
        deallocate_array_descriptor(b, descriptor);
    }
}

fn reduction_runtime_operator(operator: OpenMpReductionOperator) -> i32 {
    match operator {
        OpenMpReductionOperator::Add => 1,
        OpenMpReductionOperator::Multiply => 2,
        OpenMpReductionOperator::Max => 3,
        OpenMpReductionOperator::Min => 4,
        OpenMpReductionOperator::And => 5,
        OpenMpReductionOperator::Or => 6,
        OpenMpReductionOperator::Eqv => 7,
        OpenMpReductionOperator::Neqv => 8,
    }
}

fn scalar_reduction_identity(
    b: &mut FuncBuilder<'_>,
    operator: OpenMpReductionOperator,
    info: &LocalInfo,
) -> ValueId {
    if info.logical_kind.is_some() || info.ty == IrType::Bool {
        let identity = matches!(
            operator,
            OpenMpReductionOperator::And | OpenMpReductionOperator::Eqv
        );
        let value = b.const_bool(identity);
        return coerce_to_type(b, value, &info.ty);
    }

    let IrType::Int(width) = &info.ty else {
        unreachable!("non-integer OpenMP scalar reduction passed semantic validation")
    };
    let bits = width.bits();
    let (least, greatest) = if bits == 128 {
        (i128::MIN, i128::MAX)
    } else {
        let magnitude = 1_i128 << (bits - 1);
        (-magnitude, magnitude - 1)
    };
    let value = match operator {
        OpenMpReductionOperator::Multiply => 1,
        OpenMpReductionOperator::Max => least,
        OpenMpReductionOperator::Min => greatest,
        OpenMpReductionOperator::Add => 0,
        _ => unreachable!("logical OpenMP reduction applied to INTEGER"),
    };
    b.const_int(value, *width)
}

fn prepare_scalar_reductions(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    clauses: &[OpenMpClause],
) -> Vec<ScalarReductionBinding> {
    let mut bindings = Vec::new();
    for clause in clauses {
        let OpenMpClause::Reduction {
            operator,
            variables,
        } = clause
        else {
            continue;
        };
        for name in variables {
            let key = name.to_ascii_lowercase();
            let shared = ctx.locals.get(&key).cloned().unwrap_or_else(|| {
                panic!("validated OpenMP reduction variable '{key}' has no lowering binding")
            });
            let mut private = shared.clone();
            private.addr = b.alloca(private.ty.clone());
            private.by_ref = false;
            private.inline_const = None;
            let identity = scalar_reduction_identity(b, *operator, &private);
            b.store(identity, private.addr);
            ctx.locals.insert(key.clone(), private.clone());
            bindings.push(ScalarReductionBinding {
                name: key,
                operator: *operator,
                shared,
                private,
            });
        }
    }
    bindings
}

fn scalar_storage_address(b: &mut FuncBuilder<'_>, info: &LocalInfo) -> ValueId {
    if info.by_ref {
        b.load(info.addr)
    } else {
        info.addr
    }
}

fn finish_scalar_reductions(
    b: &mut FuncBuilder<'_>,
    bindings: &[ScalarReductionBinding],
    thread_num: ValueId,
) {
    for binding in bindings {
        let private = b.load_typed(binding.private.addr, binding.private.ty.clone());
        let private = coerce_to_type(b, private, &IrType::Int(IntWidth::I64));
        let shared_address = scalar_storage_address(b, &binding.shared);
        let original = b.load_typed(shared_address, binding.shared.ty.clone());
        let original = coerce_to_type(b, original, &IrType::Int(IntWidth::I64));
        let result_address = b.alloca(IrType::Int(IntWidth::I64));
        let operator = b.const_i32(reduction_runtime_operator(binding.operator));
        let status = b.call(
            FuncRef::External("afs_omp_reduce_i64".into()),
            vec![operator, private, original, result_address],
            IrType::Int(IntWidth::I32),
        );
        let zero = b.const_i32(0);
        let invalid = b.icmp(CmpOp::Ne, status, zero);
        let error_bb = b.create_block("omp_reduction_invalid");
        let ready_bb = b.create_block("omp_reduction_ready");
        b.cond_branch(invalid, error_bb, vec![], ready_bb, vec![]);
        b.set_block(error_bb);
        b.runtime_call(
            crate::ir::inst::RuntimeFunc::ErrorStop,
            vec![],
            IrType::Void,
        );
        b.branch(ready_bb, vec![]);

        b.set_block(ready_bb);
        let thread_zero = b.icmp(CmpOp::Eq, thread_num, zero);
        let store_bb = b.create_block("omp_reduction_store");
        let done_bb = b.create_block("omp_reduction_done");
        b.cond_branch(thread_zero, store_bb, vec![], done_bb, vec![]);
        b.set_block(store_bb);
        let result = b.load_typed(result_address, IrType::Int(IntWidth::I64));
        let result = coerce_to_type(b, result, &binding.shared.ty);
        b.store(result, shared_address);
        b.branch(done_bb, vec![]);
        b.set_block(done_bb);
    }
}

fn restore_scalar_reductions(ctx: &mut LowerCtx<'_>, bindings: &[ScalarReductionBinding]) {
    for binding in bindings {
        ctx.locals
            .insert(binding.name.clone(), binding.shared.clone());
    }
}

struct CollapsedLoopSource<'a> {
    inner_name: Option<&'a str>,
    inner_var: &'a str,
    inner_body: &'a [SpannedStmt],
    outer_lower: ValueId,
    outer_step: ValueId,
    inner_lower: ValueId,
    inner_step: ValueId,
    outer_count: ValueId,
    inner_count: ValueId,
}

fn worksharing_collapse_depth(ctx: &LowerCtx<'_>, clauses: &[OpenMpClause]) -> i64 {
    clauses
        .iter()
        .find_map(|clause| match clause {
            OpenMpClause::Collapse(depth) => Some(
                super::core::eval_const_int_in_scope_or_any_scope(
                    depth,
                    &std::collections::HashMap::new(),
                    ctx.st,
                )
                .expect("validated OpenMP COLLAPSE depth is not constant"),
            ),
            _ => None,
        })
        .unwrap_or(1)
}

fn privatize_worksharing_variable(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    var: &str,
) -> (String, LocalInfo, LocalInfo) {
    let key = var.to_ascii_lowercase();
    let mut private = ctx
        .locals
        .get(&key)
        .cloned()
        .expect("validated OpenMP DO variable has no lowering binding");
    let saved = private.clone();
    private.addr = b.alloca(private.ty.clone());
    private.by_ref = false;
    private.inline_const = None;
    ctx.locals.insert(key.clone(), private.clone());
    (key, saved, private)
}

fn lower_worksharing_loop(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    clauses: &[OpenMpClause],
    loop_stmt: &SpannedStmt,
    suppress_barrier: bool,
    precomputed_chunk: Option<ValueId>,
) {
    let Stmt::DoLoop {
        name,
        var: Some(var),
        start: Some(start),
        end: Some(end),
        step,
        body,
        ..
    } = &loop_stmt.node
    else {
        unreachable!("non-canonical OpenMP DO passed semantic validation")
    };
    let (thread_num, team_size) = ctx
        .openmp_team
        .expect("OpenMP DO lowered outside an outlined team callback");
    let nowait = clauses
        .iter()
        .any(|clause| matches!(clause, OpenMpClause::Nowait));
    let i64_ty = IrType::Int(IntWidth::I64);
    let schedule_kind = clauses
        .iter()
        .find_map(|clause| match clause {
            OpenMpClause::Schedule { kind, .. } => Some(*kind),
            _ => None,
        })
        .unwrap_or(OpenMpScheduleKind::Static);
    let dynamic_schedule = schedule_kind == OpenMpScheduleKind::Dynamic;
    let chunk_expr = clauses.iter().find_map(|clause| match clause {
        OpenMpClause::Schedule {
            kind: OpenMpScheduleKind::Static | OpenMpScheduleKind::Dynamic,
            chunk_size,
        } => chunk_size.as_ref(),
        _ => None,
    });
    let chunk_size = precomputed_chunk.or_else(|| {
        chunk_expr.map(|chunk_size| {
            let raw = super::expr::lower_expr_ctx(b, ctx, chunk_size);
            coerce_to_type(b, raw, &i64_ty)
        })
    });
    let chunk_size = if dynamic_schedule && chunk_size.is_none() {
        Some(b.const_i64(1))
    } else {
        chunk_size
    };

    // Every implicit task computes the same source iteration space before
    // replacing either associated iteration variable with private storage.
    let outer_lower_raw = super::expr::lower_expr_ctx(b, ctx, start);
    let outer_lower = coerce_to_type(b, outer_lower_raw, &i64_ty);
    let outer_upper_raw = super::expr::lower_expr_ctx(b, ctx, end);
    let outer_upper = coerce_to_type(b, outer_upper_raw, &i64_ty);
    let outer_step = if let Some(step) = step {
        let raw = super::expr::lower_expr_ctx(b, ctx, step);
        coerce_to_type(b, raw, &i64_ty)
    } else {
        b.const_i64(1)
    };

    let collapse_two = worksharing_collapse_depth(ctx, clauses) == 2;
    let mut lower = outer_lower;
    let mut upper = outer_upper;
    let mut schedule_step = outer_step;
    let mut shape_status = None;
    let collapsed = collapse_two.then(|| {
        let [inner_stmt] = body.as_slice() else {
            unreachable!("non-perfect COLLAPSE(2) nest passed semantic validation")
        };
        let Stmt::DoLoop {
            name: inner_name,
            var: Some(inner_var),
            start: Some(inner_start),
            end: Some(inner_end),
            step: inner_step,
            body: inner_body,
            ..
        } = &inner_stmt.node
        else {
            unreachable!("non-canonical COLLAPSE(2) loop passed semantic validation")
        };
        let inner_lower_raw = super::expr::lower_expr_ctx(b, ctx, inner_start);
        let inner_lower = coerce_to_type(b, inner_lower_raw, &i64_ty);
        let inner_upper_raw = super::expr::lower_expr_ctx(b, ctx, inner_end);
        let inner_upper = coerce_to_type(b, inner_upper_raw, &i64_ty);
        let inner_step = if let Some(inner_step) = inner_step {
            let raw = super::expr::lower_expr_ctx(b, ctx, inner_step);
            coerce_to_type(b, raw, &i64_ty)
        } else {
            b.const_i64(1)
        };

        let outer_count_addr = b.alloca(i64_ty.clone());
        let inner_count_addr = b.alloca(i64_ty.clone());
        let total_count_addr = b.alloca(i64_ty.clone());
        shape_status = Some(b.call(
            FuncRef::External("afs_omp_collapse2_shape".into()),
            vec![
                outer_lower,
                outer_upper,
                outer_step,
                inner_lower,
                inner_upper,
                inner_step,
                outer_count_addr,
                inner_count_addr,
                total_count_addr,
            ],
            IrType::Int(IntWidth::I32),
        ));
        let outer_count = b.load_typed(outer_count_addr, i64_ty.clone());
        let inner_count = b.load_typed(inner_count_addr, i64_ty.clone());
        let total_count = b.load_typed(total_count_addr, i64_ty.clone());
        lower = b.const_i64(0);
        let one = b.const_i64(1);
        upper = b.isub(total_count, one);
        schedule_step = one;
        CollapsedLoopSource {
            inner_name: inner_name.as_deref(),
            inner_var,
            inner_body,
            outer_lower,
            outer_step,
            inner_lower,
            inner_step,
            outer_count,
            inner_count,
        }
    });
    let reductions = prepare_scalar_reductions(b, ctx, clauses);

    let first_addr = b.alloca(i64_ty.clone());
    let last_addr = b.alloca(i64_ty.clone());
    let step_addr = b.alloca(i64_ty.clone());
    b.store(schedule_step, step_addr);
    let chunk_index_addr = if dynamic_schedule {
        None
    } else {
        chunk_size.map(|_| {
            let address = b.alloca(i64_ty.clone());
            let zero = b.const_i64(0);
            b.store(zero, address);
            address
        })
    };

    let first_name = "$afs_omp_first".to_string();
    let last_name = "$afs_omp_last".to_string();
    let step_name = "$afs_omp_step".to_string();
    let saved_first = ctx.locals.remove(&first_name);
    let saved_last = ctx.locals.remove(&last_name);
    let saved_step = ctx.locals.remove(&step_name);
    ctx.insert_scalar(first_name.clone(), first_addr, i64_ty.clone());
    ctx.insert_scalar(last_name.clone(), last_addr, i64_ty.clone());
    ctx.insert_scalar(step_name.clone(), step_addr, i64_ty);

    let (outer_key, saved_outer, private_outer) = privatize_worksharing_variable(b, ctx, var);
    let private_inner = collapsed
        .as_ref()
        .map(|collapsed| privatize_worksharing_variable(b, ctx, collapsed.inner_var));
    let collapsed_value_addrs = collapsed.as_ref().map(|_| {
        (
            b.alloca(IrType::Int(IntWidth::I64)),
            b.alloca(IrType::Int(IntWidth::I64)),
        )
    });
    let flat_name = "$afs_omp_flat".to_string();
    let flat_binding = collapsed.as_ref().map(|_| {
        let saved = ctx.locals.remove(&flat_name);
        let address = b.alloca(IrType::Int(IntWidth::I64));
        ctx.insert_scalar(flat_name.clone(), address, IrType::Int(IntWidth::I64));
        (address, saved)
    });

    let error_bb = b.create_block("omp_do_invalid");
    let dispatch_bb = b.create_block("omp_do_dispatch");
    let status_ok_bb = b.create_block("omp_do_status_ok");
    let work_bb = b.create_block("omp_do_work");
    let advance_bb =
        (dynamic_schedule || chunk_size.is_some()).then(|| b.create_block("omp_do_next_chunk"));
    let done_bb = b.create_block("omp_do_done");
    if let Some(shape_status) = shape_status {
        let zero = b.const_i32(0);
        let invalid_shape = b.icmp(CmpOp::Lt, shape_status, zero);
        b.cond_branch(invalid_shape, error_bb, vec![], dispatch_bb, vec![]);
    } else {
        b.branch(dispatch_bb, vec![]);
    }

    b.set_block(dispatch_bb);
    let status = if dynamic_schedule {
        b.call(
            FuncRef::External("afs_omp_dynamic_bounds".into()),
            vec![
                lower,
                upper,
                schedule_step,
                chunk_size.expect("dynamic schedule is missing its default chunk size"),
                first_addr,
                last_addr,
            ],
            IrType::Int(IntWidth::I32),
        )
    } else if let (Some(chunk_size), Some(chunk_index_addr)) = (chunk_size, chunk_index_addr) {
        let chunk_index = b.load_typed(chunk_index_addr, IrType::Int(IntWidth::I64));
        b.call(
            FuncRef::External("afs_omp_static_chunk_bounds".into()),
            vec![
                lower,
                upper,
                schedule_step,
                chunk_size,
                thread_num,
                team_size,
                chunk_index,
                first_addr,
                last_addr,
            ],
            IrType::Int(IntWidth::I32),
        )
    } else {
        b.call(
            FuncRef::External("afs_omp_static_bounds".into()),
            vec![
                lower,
                upper,
                schedule_step,
                thread_num,
                team_size,
                first_addr,
                last_addr,
            ],
            IrType::Int(IntWidth::I32),
        )
    };
    let zero = b.const_i32(0);
    let invalid = b.icmp(CmpOp::Lt, status, zero);
    b.cond_branch(invalid, error_bb, vec![], status_ok_bb, vec![]);

    b.set_block(error_bb);
    b.runtime_call(
        crate::ir::inst::RuntimeFunc::ErrorStop,
        vec![],
        IrType::Void,
    );
    b.branch(done_bb, vec![]);

    b.set_block(status_ok_bb);
    let has_work = b.icmp(CmpOp::Gt, status, zero);
    b.cond_branch(has_work, work_bb, vec![], done_bb, vec![]);

    b.set_block(work_bb);
    let first_expr = Some(Spanned::new(
        Expr::Name {
            name: first_name.clone(),
        },
        loop_stmt.span,
    ));
    let last_expr = Some(Spanned::new(
        Expr::Name {
            name: last_name.clone(),
        },
        loop_stmt.span,
    ));
    let step_expr = Some(Spanned::new(
        Expr::Name {
            name: step_name.clone(),
        },
        loop_stmt.span,
    ));
    let private_var_name = Some(if collapsed.is_some() {
        flat_name.clone()
    } else {
        var.clone()
    });
    let unnamed_loop = None;
    let collapsed_cycle_name = collapsed
        .as_ref()
        .and_then(|collapsed| collapsed.inner_name.map(str::to_string));
    let lowered_body = if let (
        Some(collapsed),
        Some((flat_addr, _)),
        Some((_, _, inner)),
        Some((outer_value_addr, inner_value_addr)),
    ) = (
        collapsed.as_ref(),
        flat_binding.as_ref(),
        private_inner.as_ref(),
        collapsed_value_addrs,
    ) {
        DoLoopBody::CollapsedTwo {
            flat_addr: *flat_addr,
            outer_count: collapsed.outer_count,
            inner_count: collapsed.inner_count,
            outer_addr: private_outer.addr,
            outer_ty: private_outer.ty.clone(),
            outer_lower: collapsed.outer_lower,
            outer_step: collapsed.outer_step,
            outer_value_addr,
            inner_addr: inner.addr,
            inner_ty: inner.ty.clone(),
            inner_lower: collapsed.inner_lower,
            inner_step: collapsed.inner_step,
            inner_value_addr,
            statements: collapsed.inner_body,
        }
    } else {
        DoLoopBody::Statements(body)
    };
    lower_do_loop(
        b,
        ctx,
        DoLoopFields {
            cycle_name: if collapsed.is_some() {
                &collapsed_cycle_name
            } else {
                name
            },
            exit_name: if collapsed.is_some() {
                &unnamed_loop
            } else {
                name
            },
            var: &private_var_name,
            start: &first_expr,
            end: &last_expr,
            step: &step_expr,
            body: lowered_body,
            concurrent: false,
            locality: &[],
            span: loop_stmt.span,
        },
    );
    ctx.locals.insert(outer_key, saved_outer);
    if let Some((inner_key, saved_inner, _)) = private_inner {
        ctx.locals.insert(inner_key, saved_inner);
    }
    if let Some((_, saved_flat)) = flat_binding {
        restore_temp_binding(ctx, flat_name, saved_flat);
    }
    restore_temp_binding(ctx, first_name, saved_first);
    restore_temp_binding(ctx, last_name, saved_last);
    restore_temp_binding(ctx, step_name, saved_step);
    restore_scalar_reductions(ctx, &reductions);
    if b.func().block(b.current_block()).terminator.is_none() {
        b.branch(advance_bb.unwrap_or(done_bb), vec![]);
    }

    if let (Some(advance_bb), Some(chunk_index_addr)) = (advance_bb, chunk_index_addr) {
        b.set_block(advance_bb);
        let chunk_index = b.load_typed(chunk_index_addr, IrType::Int(IntWidth::I64));
        let one = b.const_i64(1);
        let next_chunk = b.iadd(chunk_index, one);
        b.store(next_chunk, chunk_index_addr);
        b.branch(dispatch_bb, vec![]);
    } else if let Some(advance_bb) = advance_bb {
        b.set_block(advance_bb);
        b.branch(dispatch_bb, vec![]);
    }

    b.set_block(done_bb);
    finish_scalar_reductions(b, &reductions, thread_num);
    if !suppress_barrier && !nowait {
        b.call(
            FuncRef::External("afs_omp_barrier".into()),
            vec![],
            IrType::Int(IntWidth::I32),
        );
    }
}

fn restore_temp_binding(ctx: &mut LowerCtx<'_>, name: String, saved: Option<LocalInfo>) {
    ctx.locals.remove(&name);
    if let Some(saved) = saved {
        ctx.locals.insert(name, saved);
    }
}

fn materialize_shared_environment(
    b: &mut FuncBuilder<'_>,
    captures: &[Capture],
    worksharing_chunk: Option<ValueId>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> MaterializedEnvironment {
    let addressed_count = captures
        .iter()
        .filter(|capture| capture_needs_environment(capture, type_layouts))
        .count();
    let environment_slots = addressed_count + usize::from(worksharing_chunk.is_some());
    if environment_slots == 0 {
        let null = b.const_i64(0);
        return MaterializedEnvironment {
            address: b.int_to_ptr(null, IrType::Int(IntWidth::I8)),
            cleanup_descriptors: Vec::new(),
            derived_snapshots: std::collections::HashMap::new(),
            worksharing_chunk_slot: None,
        };
    }

    let environment = b.alloca(IrType::Array(
        Box::new(IrType::Int(IntWidth::I64)),
        environment_slots as u64,
    ));
    let mut cleanup_descriptors = Vec::new();
    let mut derived_snapshots = std::collections::HashMap::new();
    let mut slot_index = 0i64;
    for capture in captures {
        if !capture_needs_environment(capture, type_layouts) {
            continue;
        }
        let environment_address = if capture.kind == CaptureKind::Private
            && is_allocatable_array(&capture.info)
        {
            let source = array_descriptor_addr(b, &capture.info);
            // PRIVATE inherits only the encounter-time allocation status
            // and bounds. This byte snapshot is deliberately non-owning:
            // callbacks inspect its metadata but never read or free its
            // payload pointer.
            snapshot_array_descriptor(b, source)
        } else if capture.kind == CaptureKind::FirstPrivate && is_pointer_array(&capture.info) {
            // The environment freezes association status before the team is
            // launched. It is a non-owning descriptor snapshot: neither the
            // environment nor an implicit task may free the target.
            let source = array_descriptor_addr(b, &capture.info);
            snapshot_array_descriptor(b, source)
        } else if capture.kind == CaptureKind::FirstPrivate && is_allocatable_array(&capture.info) {
            let source = array_descriptor_addr(b, &capture.info);
            // FIRSTPRIVATE values are fixed before any implicit task can
            // run, so the environment owns a stable deep copy until the
            // synchronous join completes.
            let (snapshot, cleanup) = allocate_allocatable_private(b, &capture.info, source, true);
            cleanup_descriptors.push(cleanup);
            snapshot.addr
        } else if capture.kind == CaptureKind::FirstPrivate && is_array(&capture.info) {
            let descriptor_copy = fixed_array_uses_heap(&capture.info, b.layout);
            let source = if descriptor_copy {
                if local_uses_array_descriptor(&capture.info) {
                    array_descriptor_addr(b, &capture.info)
                } else {
                    materialize_array_descriptor_for_info(b, &capture.info)
                }
            } else {
                array_base_addr(b, &capture.info)
            };
            let (snapshot, cleanup) =
                allocate_private_array(b, &capture.info, descriptor_copy.then_some(source));
            copy_array_data(b, &snapshot, source, descriptor_copy);
            cleanup_descriptors.extend(cleanup);
            snapshot.addr
        } else if matches!(
            capture.kind,
            CaptureKind::Private | CaptureKind::FirstPrivate
        ) && capture.info.derived_type.is_some()
        {
            let outside_address = if capture.info.by_ref {
                b.load(capture.info.addr)
            } else {
                capture.info.addr
            };
            let type_name = capture
                .info
                .derived_type
                .as_deref()
                .expect("derived FIRSTPRIVATE capture lost its type name");
            let storage_ty = derived_storage_ir_type(type_name, type_layouts)
                .expect("validated OpenMP firstprivate derived scalar has no storage layout");
            let snapshot = b.alloca(storage_ty);
            let layout = type_layouts
                .get(type_name)
                .expect("validated OpenMP private derived scalar has no type layout");
            initialize_derived_storage(b, snapshot, layout, type_layouts);
            emit_derived_value_copy(b, type_layouts, type_name, snapshot, outside_address);
            let mut snapshot_info = capture.info.clone();
            snapshot_info.addr = snapshot;
            snapshot_info.by_ref = false;
            snapshot_info.inline_const = None;
            derived_snapshots.insert(capture.name.clone(), snapshot_info);
            snapshot
        } else if capture.kind == CaptureKind::FirstPrivate {
            let outside_address = if capture.info.by_ref {
                b.load(capture.info.addr)
            } else {
                capture.info.addr
            };
            // Snapshot FIRSTPRIVATE before any implicit task begins. Each
            // callback invocation copies from this stable value into its own
            // task-local slot.
            let snapshot = b.alloca(capture.info.ty.clone());
            let value = b.load_typed(outside_address, capture.info.ty.clone());
            b.store(value, snapshot);
            snapshot
        } else if shared_capture_uses_descriptor(&capture.info) {
            if local_uses_array_descriptor(&capture.info) {
                array_descriptor_addr(b, &capture.info)
            } else if capture.info.last_dim_assumed_size {
                materialize_array_section_source_descriptor(b, &capture.info)
            } else {
                materialize_array_descriptor_for_info(b, &capture.info)
            }
        } else if capture.info.by_ref {
            b.load(capture.info.addr)
        } else {
            capture.info.addr
        };
        let raw_address = b.ptr_to_int(environment_address);
        let index = b.const_i64(slot_index);
        let slot = b.gep(environment, vec![index], IrType::Int(IntWidth::I64));
        b.store(raw_address, slot);
        slot_index += 1;
    }
    let worksharing_chunk_slot = worksharing_chunk.map(|chunk_size| {
        let index = b.const_i64(slot_index);
        let slot = b.gep(environment, vec![index], IrType::Int(IntWidth::I64));
        b.store(chunk_size, slot);
        slot_index
    });
    let raw_environment = b.ptr_to_int(environment);
    MaterializedEnvironment {
        address: b.int_to_ptr(raw_environment, IrType::Int(IntWidth::I8)),
        cleanup_descriptors,
        derived_snapshots,
        worksharing_chunk_slot,
    }
}

fn install_shared_captures(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    captures: &[Capture],
) -> InstalledCaptureCleanup {
    let mut slot_index = 0i64;
    let mut cleanup = InstalledCaptureCleanup::default();
    let environment_slots = if captures
        .iter()
        .any(|capture| capture_needs_environment(capture, ctx.type_layouts))
    {
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
                if is_pointer_array(&capture.info) {
                    // OpenMP leaves a PRIVATE Fortran pointer's initial
                    // association undefined. A zero descriptor is a safe
                    // implementation value; the task owns only this
                    // association slot and never owns a target.
                    local = allocate_pointer_private(b, &capture.info, None);
                } else if is_allocatable_array(&capture.info) {
                    let index = b.const_i64(slot_index);
                    let slot = b.gep(
                        environment_slots.expect("missing OpenMP environment slots"),
                        vec![index],
                        IrType::Int(IntWidth::I64),
                    );
                    let raw_address = b.load_typed(slot, IrType::Int(IntWidth::I64));
                    let shape_snapshot = b.int_to_ptr(
                        raw_address,
                        IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392),
                    );
                    let (private, cleanup_descriptor) =
                        allocate_allocatable_private(b, &capture.info, shape_snapshot, false);
                    local = private;
                    cleanup.array_descriptors.push(cleanup_descriptor);
                    slot_index += 1;
                } else if !is_array(&capture.info) {
                    let owning_type = local.derived_type.as_deref().and_then(|name| {
                        ctx.type_layouts.get(name).and_then(|layout| {
                            derived_layout_needs_component_deallocation(layout, ctx.type_layouts)
                                .then(|| name.to_string())
                        })
                    });
                    let allocation_source = owning_type.as_deref().map(|type_name| {
                        let index = b.const_i64(slot_index);
                        let slot = b.gep(
                            environment_slots.expect("missing OpenMP environment slots"),
                            vec![index],
                            IrType::Int(IntWidth::I64),
                        );
                        let raw_address = b.load_typed(slot, IrType::Int(IntWidth::I64));
                        let storage_ty = derived_storage_ir_type(type_name, ctx.type_layouts)
                            .expect(
                                "validated OpenMP private derived scalar has no storage layout",
                            );
                        slot_index += 1;
                        b.int_to_ptr(raw_address, storage_ty)
                    });
                    let storage_ty = local
                        .derived_type
                        .as_deref()
                        .and_then(|name| derived_storage_ir_type(name, ctx.type_layouts))
                        .unwrap_or_else(|| capture.info.ty.clone());
                    local.addr = b.alloca(storage_ty);
                    local.by_ref = false;
                    local.inline_const = None;
                    if let Some(type_name) = local.derived_type.as_deref() {
                        let layout = ctx
                            .type_layouts
                            .get(type_name)
                            .expect("validated OpenMP private derived scalar has no type layout");
                        initialize_derived_storage(b, local.addr, layout, ctx.type_layouts);
                        if let Some(source) = allocation_source {
                            emit_derived_private_allocation_copy(
                                b,
                                ctx.type_layouts,
                                type_name,
                                local.addr,
                                source,
                            );
                        }
                    }
                } else {
                    let (private, cleanup_descriptor) =
                        allocate_private_array(b, &capture.info, None);
                    local = private;
                    cleanup.array_descriptors.extend(cleanup_descriptor);
                }
            }
            CaptureKind::Shared | CaptureKind::FirstPrivate => {
                let index = b.const_i64(slot_index);
                let slot = b.gep(
                    environment_slots.expect("missing OpenMP environment slots"),
                    vec![index],
                    IrType::Int(IntWidth::I64),
                );
                let raw_address = b.load_typed(slot, IrType::Int(IntWidth::I64));
                let captures_descriptor = capture.kind == CaptureKind::Shared
                    && shared_capture_uses_descriptor(&capture.info);
                let firstprivate_array =
                    capture.kind == CaptureKind::FirstPrivate && is_array(&capture.info);
                let firstprivate_allocatable =
                    firstprivate_array && is_allocatable_array(&capture.info);
                let firstprivate_pointer = firstprivate_array && is_pointer_array(&capture.info);
                let firstprivate_descriptor = firstprivate_array
                    && (firstprivate_pointer
                        || firstprivate_allocatable
                        || fixed_array_uses_heap(&capture.info, b.layout));
                let captured_pointee = if captures_descriptor || firstprivate_descriptor {
                    IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 392)
                } else if firstprivate_array {
                    fixed_array_storage_type(&capture.info, b.layout)
                } else if capture.kind == CaptureKind::FirstPrivate {
                    capture
                        .info
                        .derived_type
                        .as_deref()
                        .and_then(|name| derived_storage_ir_type(name, ctx.type_layouts))
                        .unwrap_or_else(|| capture.info.ty.clone())
                } else {
                    capture.info.ty.clone()
                };
                let environment_address = b.int_to_ptr(raw_address, captured_pointee);
                if firstprivate_pointer {
                    local = allocate_pointer_private(b, &capture.info, Some(environment_address));
                } else if firstprivate_allocatable {
                    let (private, cleanup_descriptor) =
                        allocate_allocatable_private(b, &capture.info, environment_address, true);
                    local = private;
                    cleanup.array_descriptors.push(cleanup_descriptor);
                } else if firstprivate_array {
                    let shape_descriptor = firstprivate_descriptor.then_some(environment_address);
                    let (private, cleanup_descriptor) =
                        allocate_private_array(b, &capture.info, shape_descriptor);
                    copy_array_data(b, &private, environment_address, firstprivate_descriptor);
                    local = private;
                    cleanup.array_descriptors.extend(cleanup_descriptor);
                } else if capture.kind == CaptureKind::FirstPrivate {
                    let storage_ty = capture
                        .info
                        .derived_type
                        .as_deref()
                        .and_then(|name| derived_storage_ir_type(name, ctx.type_layouts))
                        .unwrap_or_else(|| capture.info.ty.clone());
                    let private = b.alloca(storage_ty);
                    if let Some(type_name) = capture.info.derived_type.as_deref() {
                        let layout = ctx.type_layouts.get(type_name).expect(
                            "validated OpenMP firstprivate derived scalar has no type layout",
                        );
                        initialize_derived_storage(b, private, layout, ctx.type_layouts);
                        emit_derived_value_copy(
                            b,
                            ctx.type_layouts,
                            type_name,
                            private,
                            environment_address,
                        );
                    } else {
                        let value = b.load_typed(environment_address, capture.info.ty.clone());
                        b.store(value, private);
                    }
                    local.addr = private;
                    local.inline_const = None;
                } else {
                    local.addr = environment_address;
                    if captures_descriptor {
                        // Runtime-bound explicit-shape dummies carry bound SSA
                        // values in the encountering function. The descriptor
                        // materialized above is their complete cross-function
                        // view; never leak those parent ValueIds into the
                        // outlined callback. Preserve the vector length because
                        // descriptor-backed allocatables and pointers encode
                        // their declared rank there when `dims` is empty.
                        local.descriptor_arg = true;
                        local.runtime_dim_upper.fill(None);
                    }
                }
                local.by_ref = false;
                slot_index += 1;
            }
        }
        if matches!(
            capture.kind,
            CaptureKind::Private | CaptureKind::FirstPrivate
        ) && !is_array(&local)
            && !local.allocatable
            && !local.is_pointer
            && local.derived_type.is_some()
        {
            cleanup
                .private_derived_scalars
                .insert(capture.name.clone(), local.clone());
        }
        ctx.locals.insert(capture.name.clone(), local);
    }
    cleanup
}
