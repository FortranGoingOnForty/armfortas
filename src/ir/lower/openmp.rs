//! Lowering for executable OpenMP constructs.
//!
//! Semantic validation currently admits capture-free `PARALLEL` regions,
//! numeric/logical scalar data, supported shared numeric/logical arrays, and
//! constant explicit-shape, allocatable, and pointer private/firstprivate
//! numeric/logical arrays.
//! Each region is outlined into the fixed callback shape owned by the
//! ARMFORTAS OpenMP ABI and synchronously invoked through the runtime. Shared
//! addresses, shared owning/non-owning array descriptors, and firstprivate
//! snapshots live in a compiler-private environment whose lifetime is bounded
//! by the synchronous join. Private objects live in each callback invocation,
//! using inline storage below the compiler's stack threshold and owned
//! descriptors above it.

use crate::ast::openmp::{OpenMpClause, OpenMpConstruct};
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::{CmpOp, FuncRef, Function, Param, ValueId};
use crate::ir::types::{IntWidth, IrType};

use super::alloc::rewrite_heap_promoted_declared_bounds;
use super::core::{
    array_base_addr, array_descriptor_addr, collect_format_labels, collect_label_blocks,
    emit_memcpy_bytes, ensure_termination, ir_scalar_byte_size, local_declared_rank,
    local_uses_array_descriptor, materialize_array_descriptor_for_info,
    materialize_array_section_source_descriptor,
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

fn capture_needs_environment(capture: &Capture) -> bool {
    matches!(
        capture.kind,
        CaptureKind::Shared | CaptureKind::FirstPrivate
    ) || (capture.kind == CaptureKind::Private && is_allocatable_array(&capture.info))
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
            let cleanup_descriptors =
                install_shared_captures(&mut outlined, &mut outlined_ctx, &captures);
            collect_label_blocks(&mut outlined, body, &mut outlined_ctx.label_blocks);
            collect_format_labels(body, &mut outlined_ctx.format_labels);
            let _scope = ProcScopeGuard::enter(outlined_ctx.proc_scope_id);
            super::stmt::lower_stmts(&mut outlined, &mut outlined_ctx, body);
            if outlined
                .func()
                .block(outlined.current_block())
                .terminator
                .is_none()
            {
                for descriptor in cleanup_descriptors {
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
    for descriptor in environment.cleanup_descriptors {
        deallocate_array_descriptor(b, descriptor);
    }
}

fn materialize_shared_environment(
    b: &mut FuncBuilder<'_>,
    captures: &[Capture],
) -> MaterializedEnvironment {
    let addressed_count = captures
        .iter()
        .filter(|capture| capture_needs_environment(capture))
        .count();
    if addressed_count == 0 {
        let null = b.const_i64(0);
        return MaterializedEnvironment {
            address: b.int_to_ptr(null, IrType::Int(IntWidth::I8)),
            cleanup_descriptors: Vec::new(),
        };
    }

    let environment = b.alloca(IrType::Array(
        Box::new(IrType::Int(IntWidth::I64)),
        addressed_count as u64,
    ));
    let mut cleanup_descriptors = Vec::new();
    let mut slot_index = 0i64;
    for capture in captures {
        if !capture_needs_environment(capture) {
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
    let raw_environment = b.ptr_to_int(environment);
    MaterializedEnvironment {
        address: b.int_to_ptr(raw_environment, IrType::Int(IntWidth::I8)),
        cleanup_descriptors,
    }
}

fn install_shared_captures(
    b: &mut FuncBuilder<'_>,
    ctx: &mut LowerCtx<'_>,
    captures: &[Capture],
) -> Vec<ValueId> {
    let mut slot_index = 0i64;
    let mut cleanup_descriptors = Vec::new();
    let environment_slots = if captures.iter().any(capture_needs_environment) {
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
                    let (private, cleanup) =
                        allocate_allocatable_private(b, &capture.info, shape_snapshot, false);
                    local = private;
                    cleanup_descriptors.push(cleanup);
                    slot_index += 1;
                } else if !is_array(&capture.info) {
                    local.addr = b.alloca(capture.info.ty.clone());
                    local.by_ref = false;
                    local.inline_const = None;
                } else {
                    let (private, cleanup) = allocate_private_array(b, &capture.info, None);
                    local = private;
                    cleanup_descriptors.extend(cleanup);
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
                } else {
                    capture.info.ty.clone()
                };
                let environment_address = b.int_to_ptr(raw_address, captured_pointee);
                if firstprivate_pointer {
                    local = allocate_pointer_private(b, &capture.info, Some(environment_address));
                } else if firstprivate_allocatable {
                    let (private, cleanup) =
                        allocate_allocatable_private(b, &capture.info, environment_address, true);
                    local = private;
                    cleanup_descriptors.push(cleanup);
                } else if firstprivate_array {
                    let shape_descriptor = firstprivate_descriptor.then_some(environment_address);
                    let (private, cleanup) =
                        allocate_private_array(b, &capture.info, shape_descriptor);
                    copy_array_data(b, &private, environment_address, firstprivate_descriptor);
                    local = private;
                    cleanup_descriptors.extend(cleanup);
                } else if capture.kind == CaptureKind::FirstPrivate {
                    let private = b.alloca(capture.info.ty.clone());
                    let value = b.load_typed(environment_address, capture.info.ty.clone());
                    b.store(value, private);
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
        ctx.locals.insert(capture.name.clone(), local);
    }
    cleanup_descriptors
}
