//! Lowering of Fortran statements (Stmt::*) to IR.
//!
//! Extracted from `core.rs` in Sprint 11 Stage C. Pure mechanical
//! move — behavior unchanged. The dispatcher still matches on the
//! 41 Stmt variants; future sub-stages may split per-variant.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::ast::expr::Expr;
use crate::ast::stmt::*;
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;

use super::core::*;
use super::ctx::{CharKind, HiddenResultAbi, LocalInfo, LowerCtx};
use super::helpers::coerce_to_type;

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
                                } else if local_uses_array_descriptor(&info)
                                    && local_declared_rank(&info) == 0
                                    && info.derived_type.is_some()
                                    && ctx
                                        .st
                                        .find_symbol_any_scope(&key)
                                        .and_then(|sym| sym.type_info.as_ref())
                                        .is_some_and(|ti| {
                                            matches!(ti, crate::sema::symtab::TypeInfo::Derived(_))
                                        })
                                {
                                    let desc = array_descriptor_addr(b, &info);
                                    let allocated = b.call(
                                        FuncRef::External("afs_allocated".into()),
                                        vec![desc],
                                        IrType::Int(IntWidth::I32),
                                    );
                                    let zero32 = b.const_i32(0);
                                    let needs_alloc = b.icmp(CmpOp::Eq, allocated, zero32);
                                    let alloc_bb = b.create_block("scalar_derived_assign_alloc");
                                    let copy_bb = b.create_block("scalar_derived_assign_copy");
                                    let done_bb = b.create_block("scalar_derived_assign_done");
                                    b.cond_branch(
                                        needs_alloc,
                                        alloc_bb,
                                        vec![],
                                        copy_bb,
                                        vec![],
                                    );

                                    b.set_block(alloc_bb);
                                    if let Some(ref tn) = info.derived_type {
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

                                    b.set_block(copy_bb);
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let dest = derived_storage_addr(b, &info);
                                    if let Some(ref tn) = info.derived_type {
                                        emit_derived_value_copy(b, ctx.type_layouts, tn, dest, val);
                                    }
                                    // Scalar descriptor-backed TYPE allocatables keep their
                                    // dynamic type identity in the descriptor sidecar, so
                                    // constructor/function-result assignment must restamp the
                                    // concrete metadata after copying the value bytes.
                                    if let Some(tag) = derived_type_tag_value(
                                        b,
                                        info.derived_type.as_deref(),
                                        ctx.type_layouts,
                                    ) {
                                        store_array_desc_type_tag(b, desc, tag);
                                    }
                                    if let Some(lookup) = derived_type_tbp_lookup_value(
                                        b,
                                        info.derived_type.as_deref(),
                                        ctx.type_layouts,
                                    ) {
                                        store_array_desc_tbp_lookup_ptr(b, desc, lookup);
                                    }
                                    b.branch(done_bb, vec![]);
                                    b.set_block(done_bb);
                                } else if !info.dims.is_empty() || info.allocatable {
                                    if try_lower_elemental_array_assign(b, ctx, name, &info, value)
                                    {
                                        return;
                                    }
                                    if let Expr::FunctionCall {
                                        callee,
                                        args: call_args,
                                    } = &value.node
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
                                                let direct_elemental =
                                                    is_elemental_math_intrinsic(cname)
                                                        || ctx.elemental_funcs.contains(&lname)
                                                        || ctx
                                                            .st
                                                            .find_symbol_any_scope(&lname)
                                                            .is_some_and(|s| s.attrs.elemental);
                                                let generic_specifics_elemental = !direct_elemental
                                                    && named_interface_specifics(ctx.st, &lname)
                                                        .map(|specs| {
                                                            !specs.is_empty()
                                                                && specs.iter().all(|s| {
                                                                    ctx.elemental_funcs
                                                                        .contains(&s.to_lowercase())
                                                                        || ctx
                                                                            .st
                                                                            .find_symbol_any_scope(s)
                                                                            .is_some_and(|sym| {
                                                                                sym.attrs.elemental
                                                                            })
                                                                })
                                                        })
                                                        .unwrap_or(false);
                                                let is_elemental = direct_elemental
                                                    || generic_specifics_elemental;
                                                is_elemental
                                                    && call_args.iter().any(|arg| {
                                                        matches!(
                                                            &arg.value,
                                                            crate::ast::expr::SectionSubscript::Element(e)
                                                                if expr_contains_array_refs(e, &ctx.locals)
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
                                                )
                                                || (
                                                    // sum(arr, dim) is rank-N-1: route to
                                                    // lower_array_assign so the sum-dim arm
                                                    // in lower_array_expr_descriptor fills
                                                    // the result descriptor. Plain sum(arr)
                                                    // is scalar; that arm returns None and
                                                    // assignment falls through to scalar
                                                    // broadcast.
                                                    lname == "sum"
                                                    && call_args.iter().enumerate().any(|(i, a)| {
                                                        let kw = a.keyword.as_deref().map(|s| s.to_lowercase());
                                                        matches!(kw.as_deref(), Some("dim"))
                                                            || (i == 1 && kw.is_none())
                                                    })
                                                )
                                                || (
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
                                                    && call_args.iter().enumerate().any(|(i, a)| {
                                                        let kw = a.keyword.as_deref().map(|s| s.to_lowercase());
                                                        matches!(kw.as_deref(), Some("dim"))
                                                            || (i == 1 && kw.is_none())
                                                    })
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
                                                    if let Expr::Name { name: cname } = &callee.node {
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
                                                    } else {
                                                        false
                                                    }
                                                };
                                        if callee_is_local_array
                                            || callee_is_elemental_array_intrinsic
                                            || callee_is_transformational_intrinsic
                                            || callee_is_scalar_broadcast_intrinsic
                                        {
                                            lower_array_assign(b, ctx, name, &info, value);
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
                                                // expects a 384-byte descriptor — handing it
                                                // the buffer corrupts the caller frame the
                                                // moment the callee touches dims/flags. Allocate
                                                // a real descriptor temp, call into it, copy
                                                // the bytes back, and deallocate the heap
                                                // result.
                                                if local_uses_array_descriptor(&info) {
                                                    let dest_desc = array_descriptor_addr(b, &info);
                                                    lower_alloc_return_call_into_desc(
                                                        b,
                                                        ctx,
                                                        dest_desc,
                                                        callee_name,
                                                        call_args,
                                                    );
                                                } else {
                                                    let tmp_desc = b.alloca(IrType::Array(
                                                        Box::new(IrType::Int(IntWidth::I8)),
                                                        384,
                                                    ));
                                                    let zero32 = b.const_i32(0);
                                                    let sz384 = b.const_i64(384);
                                                    b.call(
                                                        FuncRef::External("memset".into()),
                                                        vec![tmp_desc, zero32, sz384],
                                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                                    );
                                                    lower_alloc_return_call_into_desc(
                                                        b,
                                                        ctx,
                                                        tmp_desc,
                                                        callee_name,
                                                        call_args,
                                                    );
                                                    let n = array_total_elems_value(b, &info);
                                                    let elem_bytes = b.const_i64(
                                                        ir_scalar_byte_size(&info.ty),
                                                    );
                                                    let byte_count = b.imul(n, elem_bytes);
                                                    let src_base = b.load_typed(
                                                        tmp_desc,
                                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                                    );
                                                    b.call(
                                                        FuncRef::External("memcpy".into()),
                                                        vec![info.addr, src_base, byte_count],
                                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                                    );
                                                    let stat = b.alloca(IrType::Int(IntWidth::I32));
                                                    b.store(zero32, stat);
                                                    b.call(
                                                        FuncRef::External("afs_deallocate_array".into()),
                                                        vec![tmp_desc, stat],
                                                        IrType::Void,
                                                    );
                                                }
                                            } else {
                                                // Function returns a temp descriptor. Mirror
                                                // the alloc_return path: when dest is a real
                                                // descriptor, route through afs_assign_allocatable;
                                                // when dest is a fixed-shape buffer, memcpy the
                                                // result bytes in.
                                                let src_desc = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                                if local_uses_array_descriptor(&info) {
                                                    let dest_desc = array_descriptor_addr(b, &info);
                                                    b.call(
                                                        FuncRef::External(
                                                            "afs_assign_allocatable".into(),
                                                        ),
                                                        vec![dest_desc, src_desc],
                                                        IrType::Void,
                                                    );
                                                } else {
                                                    let n = array_total_elems_value(b, &info);
                                                    let elem_bytes = b.const_i64(
                                                        ir_scalar_byte_size(&info.ty),
                                                    );
                                                    let byte_count = b.imul(n, elem_bytes);
                                                    let src_base = b.load_typed(
                                                        src_desc,
                                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                                    );
                                                    b.call(
                                                        FuncRef::External("memcpy".into()),
                                                        vec![info.addr, src_base, byte_count],
                                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
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
                                            // Indirect callee: same dest split as above.
                                            let src_desc = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                            if local_uses_array_descriptor(&info) {
                                                let dest_desc = array_descriptor_addr(b, &info);
                                                b.call(
                                                    FuncRef::External("afs_assign_allocatable".into()),
                                                    vec![dest_desc, src_desc],
                                                    IrType::Void,
                                                );
                                            } else {
                                                let n = array_total_elems_value(b, &info);
                                                let elem_bytes = b.const_i64(
                                                    ir_scalar_byte_size(&info.ty),
                                                );
                                                let byte_count = b.imul(n, elem_bytes);
                                                let src_base = b.load_typed(
                                                    src_desc,
                                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                                );
                                                b.call(
                                                    FuncRef::External("memcpy".into()),
                                                    vec![info.addr, src_base, byte_count],
                                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
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
                                    let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let dest = derived_storage_addr(b, &info);
                                    if let Some(ref tn) = info.derived_type {
                                        emit_derived_value_copy(b, ctx.type_layouts, tn, dest, val);
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
                                    let src = if matches!(&src_ty, Some(t) if is_complex_ty(t)) {
                                        raw
                                    } else {
                                        let fw = complex_float_width(&info.ty);
                                        materialize_complex_operand(b, raw, fw)
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
                                } else if info.is_class
                                    && info.dims.is_empty()
                                    && ctx.st
                                        .find_symbol_any_scope(&key)
                                        .map(|s| s.attrs.allocatable)
                                        .unwrap_or(false)
                                    && matches!(value.node, Expr::ComponentAccess { .. })
                                {
                                    // Scalar polymorphic allocatable assign:
                                    // `class(*), allocatable :: out; out = h%poly_field`
                                    // — copy the 384-byte descriptor verbatim
                                    // when the RHS is a polymorphic component
                                    // access.  Avoids the scalar-store
                                    // fallback that would truncate the source
                                    // descriptor's payload to a single i32.
                                    let dst = array_descriptor_addr(b, &info);
                                    let src_desc_opt: Option<ValueId> =
                                        resolve_component_field_access(
                                            b,
                                            &ctx.locals,
                                            value,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                        .map(|(p, _)| p);
                                    if let Some(src) = src_desc_opt {
                                        let sz = b.const_i64(384);
                                        b.call(
                                            FuncRef::External("memcpy".into()),
                                            vec![dst, src, sz],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                    } else {
                                        // Fall through if we can't resolve.
                                        let val = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                        let coerced = coerce_to_type(b, val, &info.ty);
                                        let ptr = b.load(info.addr);
                                        b.store(coerced, ptr);
                                    }
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
                            // Vector subscript: a([i1, i2, ...]) = scalar
                            // Expand to scalar assignments a(i1) = scalar, etc.
                            if local_is_array_like(&info)
                                && !is_scalar_fixed_alloc_char
                                && args.len() == 1
                            {
                                if let crate::ast::expr::SectionSubscript::Element(idx_expr) =
                                    &args[0].value
                                {
                                    if let Expr::ArrayConstructor { values: idx_values, .. } =
                                        &idx_expr.node
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
                                        && expr_returns_array(
                                            idx_expr,
                                            &ctx.locals,
                                            ctx.st,
                                        )
                                        && lower_dynamic_vector_subscript_assign(
                                            b,
                                            ctx,
                                            &info,
                                            idx_expr,
                                            value,
                                        )
                                    {
                                        return;
                                    }
                                }
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
                                lower_array_assign(b, ctx, "", &whole_view, value);
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
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                Expr::ComponentAccess { base, component } => {
                    // x%field = val (supports chained: x%a%b = val).
                    if let Some(info) = component_intrinsic_local_info(
                        b,
                        &ctx.locals,
                        target,
                        ctx.st,
                        ctx.type_layouts,
                    ) {
                        if local_is_array_like(&info) {
                            lower_array_assign(b, ctx, "", &info, value);
                            return;
                        }
                    }
                    if let Some((base_addr, type_name)) =
                        resolve_component_base(b, &ctx.locals, base, ctx.st, ctx.type_layouts)
                    {
                        if let Some(layout) = ctx.type_layouts.get(&type_name) {
                            if let Some(field) = layout.field(component) {
                                let offset = b.const_i64(field.offset as i64);
                                let field_ptr =
                                    b.gep(base_addr, vec![offset], IrType::Int(IntWidth::I8));

                                // Character field: copy string data with space padding.
                                if let CharKind::Fixed(flen) = field_char_kind(field) {
                                    let (src_ptr, src_len) = lower_string_expr_ctx(b, ctx, value);
                                    let dest_len = b.const_i64(flen);
                                    b.call(
                                        FuncRef::External("afs_assign_char_fixed".into()),
                                        vec![field_ptr, dest_len, src_ptr, src_len],
                                        IrType::Void,
                                    );
                                } else if is_deferred_char_component_field(field) {
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
                                } else if matches!(
                                    field.type_info,
                                    crate::sema::symtab::TypeInfo::Derived(_)
                                ) && field.allocatable
                                    && field.size == 384
                                    && field.dims.is_empty()
                                {
                                    let Some(type_name) = field_derived_type_name(field) else {
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
                                    if let Some(layout) = ctx.type_layouts.get(&type_name) {
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
                                    let src_ptr = super::expr::lower_expr_ctx_tl(b, ctx, value);
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
                                    if let Some(lookup) = derived_type_tbp_lookup_value(
                                        b,
                                        Some(type_name.as_str()),
                                        ctx.type_layouts,
                                    ) {
                                        store_array_desc_tbp_lookup_ptr(b, desc, lookup);
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
                                    let src_ptr = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    if let Some(nested_name) = field_derived_type_name(field) {
                                        emit_derived_value_copy(
                                            b,
                                            ctx.type_layouts,
                                            &nested_name,
                                            field_ptr,
                                            src_ptr,
                                        );
                                    }
                                } else if is_complex_ty(&type_info_to_ir_type(&field.type_info))
                                    && !field.pointer
                                    && !field.allocatable
                                    && field.dims.is_empty()
                                {
                                    let raw = super::expr::lower_expr_ctx_tl(b, ctx, value);
                                    let field_ir_ty = type_info_to_ir_type(&field.type_info);
                                    let src_ty = b.func().value_type(raw);
                                    let src = if matches!(&src_ty, Some(t) if is_complex_ty(t)) {
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
                                } else if matches!(
                                    field.type_info,
                                    crate::sema::symtab::TypeInfo::ClassStar
                                        | crate::sema::symtab::TypeInfo::TypeStar
                                ) && field.allocatable
                                    && field.dims.is_empty()
                                {
                                    // Polymorphic component target:
                                    // `derived%poly_field = expr` where
                                    // `poly_field` is class(*), allocatable.
                                    // Memcpy the source descriptor.  RHS is
                                    // expected to be a polymorphic local or
                                    // another class(*) component access.
                                    let src_desc_opt: Option<ValueId> =
                                        match &value.node {
                                            Expr::ComponentAccess { .. } => {
                                                resolve_component_field_access(
                                                    b,
                                                    &ctx.locals,
                                                    value,
                                                    ctx.st,
                                                    ctx.type_layouts,
                                                )
                                                .map(|(p, _)| p)
                                            }
                                            Expr::Name { name } => ctx
                                                .locals
                                                .get(&name.to_lowercase())
                                                .filter(|info| {
                                                    info.is_class && info.dims.is_empty()
                                                })
                                                .map(|info| {
                                                    array_descriptor_addr(b, info)
                                                }),
                                            _ => None,
                                        };
                                    if let Some(src) = src_desc_opt {
                                        let sz = b.const_i64(384);
                                        b.call(
                                            FuncRef::External("memcpy".into()),
                                            vec![field_ptr, src, sz],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                    } else {
                                        // Last-resort: skip the assignment
                                        // rather than emit invalid IR.
                                        // Better than truncating to i32.
                                    }
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

        Stmt::Print { items, .. } => {
            // PRINT * → unit 6 (stdout).
            let unit = b.const_i32(6);
            lower_write_items(b, ctx, items, unit);
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
                    let (p, l) = lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &c.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    );
                    Some(b.call(
                        FuncRef::External("afs_advance_eval".into()),
                        vec![p, l],
                        IrType::Int(IntWidth::I32),
                    ))
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
            let iostat_ptr = iostat_ctrl
                .map(|c| lower_arg_by_ref_ctx(b, ctx, &c.value))
                .unwrap_or(null_i8_ptr);
            let (iomsg_ptr, iomsg_len) = if let Some(c) = iomsg_ctrl {
                lower_string_expr_with_layouts(
                    b,
                    &ctx.locals,
                    &c.value,
                    ctx.st,
                    Some(ctx.type_layouts),
                )
            } else {
                (null_i8_ptr, zero_i64)
            };

            if let Some(ctrl) = controls.first() {
                if let Some((buf_ptr, buf_len)) = internal_io_buffer(b, ctx, ctrl) {
                    if is_list_directed {
                        lower_internal_write_items(b, ctx, items, buf_ptr, buf_len);
                    } else {
                        let (fmt_ptr, fmt_len) = lower_string_expr_with_layouts(
                            b,
                            &ctx.locals,
                            &fmt_control.unwrap().value,
                            ctx.st,
                            Some(ctx.type_layouts),
                        );
                        b.call(
                            FuncRef::External("afs_fmt_begin_internal_ex".into()),
                            vec![
                                buf_ptr, buf_len, fmt_ptr, fmt_len, iostat_ptr, iomsg_ptr,
                                iomsg_len,
                            ],
                            IrType::Void,
                        );
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
                let adv = advance_runtime
                    .unwrap_or_else(|| b.const_i32(if advance { 1 } else { 0 }));
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
                let (fmt_ptr, fmt_len) = lower_string_expr_with_layouts(
                    b,
                    &ctx.locals,
                    &fmt_control.unwrap().value,
                    ctx.st,
                    Some(ctx.type_layouts),
                );
                b.call(
                    FuncRef::External("afs_fmt_begin_ex".into()),
                    vec![unit, fmt_ptr, fmt_len, iostat_ptr, iomsg_ptr, iomsg_len],
                    IrType::Void,
                );

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
                if let Some((target, closure_args, signature_key)) =
                    procedure_pointer_component_call_target(
                        b,
                        &ctx.locals,
                        callee,
                        ctx.st,
                        ctx.type_layouts,
                    )
                {
                    let arg_slots = reorder_args_by_keyword_slots(args, &signature_key, ctx.st);
                    let abi_lookup_keys = procedure_abi_lookup_keys(ctx.st, &[&signature_key]);
                    let abi_primary_key = abi_lookup_keys
                        .first()
                        .map(String::as_str)
                        .unwrap_or(signature_key.as_str());
                    let value_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_value_arg_mask(ctx.st, k)
                    });
                    let desc_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.descriptor_params, k)
                    });
                    let bind_c_char_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_bind_c_char_arg_mask(ctx.st, k)
                    });
                    let pointer_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_pointer_arg_mask(ctx.st, k)
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
                    let mut arg_vals: Vec<ValueId> = Vec::with_capacity(arg_slots.len());
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
                        let wants_pointer = pointer_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_string_descriptor = string_desc_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_polymorphic_descriptor = class_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_string_descriptor =
                            wants_string_descriptor && !wants_bind_c_char;
                        let value = match slot {
                            Some(arg) => match &arg.value {
                                crate::ast::expr::SectionSubscript::Element(e) => {
                                    let wants_descriptor = (mask_wants_descriptor
                                        || actual_is_descriptor_array(&ctx.locals, e))
                                        && !wants_bind_c_char;
                                    if is_value && wants_bind_c_char {
                                        lower_bind_c_char_value_arg(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        )
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
                                        lower_arg_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            wants_polymorphic_descriptor,
                                        )
                                    } else if wants_string_descriptor {
                                        lower_arg_string_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                        )
                                    } else if wants_bind_c_char {
                                        lower_bind_c_char_arg_raw(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
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
                                        .unwrap_or_else(|| lower_arg_by_ref_ctx(b, ctx, e))
                                    } else {
                                        lower_arg_by_ref_ctx(b, ctx, e)
                                    }
                                }
                                _ => b.const_i32(0),
                            },
                            None => {
                                missing_optional_call_arg(b, ctx.st, abi_primary_key, i, is_value)
                            }
                        };
                        arg_vals.push(value);
                    }
                    if let Some(opt_flags) = opt_flags {
                        for flag in opt_flags.iter().skip(arg_vals.len()) {
                            if *flag {
                                arg_vals.push(b.const_i64(0));
                            }
                        }
                    }
                    if let Some(cls_flags) = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.char_len_star_params, k)
                            .or_else(|| callee_char_len_star_mask(ctx.st, k))
                    }) {
                        for (i, flag) in cls_flags.iter().enumerate() {
                            if !*flag || i >= arg_slots.len() {
                                continue;
                            }
                            if let Some(arg) = &arg_slots[i] {
                                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                                    arg_vals.push(
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
                                        .unwrap_or_else(|| b.const_i64(0)),
                                    );
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
                // Try intrinsic subroutine lowering first.
                if !super::intrinsic_sub::lower_intrinsic_subroutine(b, ctx, &key, args) {
                    let procptr_target =
                        procedure_pointer_call_target(b, &ctx.locals, ctx.st, &key);
                    let signature_key = procptr_target
                        .as_ref()
                        .map(|(_, sig_key)| sig_key.clone())
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
                    let arg_slots = reorder_args_by_keyword_slots(
                        args,
                        if procptr_target.is_some() {
                            &signature_key
                        } else {
                            &resolved_key
                        },
                        ctx.st,
                    );
                    let abi_lookup_keys = procedure_abi_lookup_keys(
                        ctx.st,
                        &[resolved_name.as_str(), &resolved_key, &signature_key, &key],
                    );
                    let abi_primary_key = abi_lookup_keys
                        .first()
                        .map(String::as_str)
                        .unwrap_or(resolved_key.as_str());
                    let value_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_value_arg_mask(ctx.st, k)
                    });
                    let desc_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.descriptor_params, k)
                    });
                    let bind_c_char_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_bind_c_char_arg_mask(ctx.st, k)
                    });
                    let pointer_mask = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_pointer_arg_mask(ctx.st, k)
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
                    let mut arg_vals: Vec<ValueId> = Vec::with_capacity(arg_slots.len());
                    for (i, slot) in arg_slots.iter().enumerate() {
                        let is_value = value_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_descriptor = desc_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_bind_c_char = bind_c_char_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_pointer = pointer_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_string_descriptor = string_desc_mask
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                        let wants_descriptor = wants_descriptor && !wants_bind_c_char;
                        let wants_polymorphic_descriptor = wants_descriptor
                            && class_mask
                                .as_ref()
                                .map(|mask| mask.get(i).copied().unwrap_or(false))
                                .unwrap_or(false);
                        let wants_string_descriptor =
                            wants_string_descriptor && !wants_bind_c_char;
                        let value = match slot {
                            Some(arg) => match &arg.value {
                                crate::ast::expr::SectionSubscript::Element(e) => {
                                    if is_value && wants_bind_c_char {
                                        lower_bind_c_char_value_arg(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
                                        )
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
                                        lower_arg_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            wants_polymorphic_descriptor,
                                        )
                                    } else if wants_string_descriptor {
                                        lower_arg_string_descriptor(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                        )
                                    } else if wants_bind_c_char {
                                        lower_bind_c_char_arg_raw(
                                            b,
                                            &ctx.locals,
                                            e,
                                            ctx.st,
                                            Some(ctx.type_layouts),
                                            Some(ctx.internal_funcs),
                                            Some(ctx.contained_host_refs),
                                            Some(ctx.descriptor_params),
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
                                        .unwrap_or_else(|| lower_arg_by_ref_ctx(b, ctx, e))
                                    } else {
                                        lower_arg_by_ref_ctx(b, ctx, e)
                                    }
                                }
                                _ => b.const_i32(0),
                            },
                            None => {
                                missing_optional_call_arg(b, ctx.st, abi_primary_key, i, is_value)
                            }
                        };
                        arg_vals.push(value);
                    }
                    if let Some(opt_flags) = opt_flags {
                        for flag in opt_flags.iter().skip(arg_vals.len()) {
                            if *flag {
                                arg_vals.push(b.const_i64(0)); // null → absent
                            }
                        }
                    }
                    // Hidden character-length ABI: for each callee
                    // param that is character(len=*), append the
                    // actual argument's string length as an i64.
                    if let Some(cls_flags) = first_procedure_lookup(&abi_lookup_keys, |k| {
                        cached_param_mask_for_lookup(ctx.st, ctx.char_len_star_params, k)
                            .or_else(|| callee_char_len_star_mask(ctx.st, k))
                    }) {
                        for (i, flag) in cls_flags.iter().enumerate() {
                            if !*flag || i >= arg_slots.len() {
                                continue;
                            }
                            if let Some(arg) = &arg_slots[i] {
                                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                                    arg_vals.push(
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
                                        .unwrap_or_else(|| b.const_i64(0)),
                                    );
                                } else {
                                    arg_vals.push(b.const_i64(0));
                                }
                            } else {
                                arg_vals.push(b.const_i64(0));
                            }
                        }
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
                    let func_ref = if let Some((target, _)) = procptr_target {
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
            locality: _,
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
            // WHERE(mask) body [ELSEWHERE body] END WHERE
            // Collect ALL array names referenced in mask, body, OR
            // elsewhere body. Missing the elsewhere arm caused a
            // silent miscompile: an array reference appearing only
            // in elsewhere (e.g., `where (a > 0) c = a; elsewhere; c
            // = d`) was not scalarized, so `c = d` lowered through
            // the scalar path and silently produced 0.0 instead of
            // d(i).
            let mut array_names: Vec<String> = Vec::new();
            collect_array_names(mask, &ctx.locals, &mut array_names);
            for s in body {
                collect_array_names_stmt(s, &ctx.locals, &mut array_names);
            }
            if let Some((_emask, ebody)) = elsewhere.first() {
                for s in ebody {
                    collect_array_names_stmt(s, &ctx.locals, &mut array_names);
                }
            }

            if array_names.is_empty() {
                // No arrays — fall back to scalar IF-THEN-ELSE.
                let cond = super::expr::lower_expr_ctx_tl(b, ctx, mask);
                let bb_then = b.create_block("where_then");
                let bb_else = if !elsewhere.is_empty() {
                    Some(b.create_block("where_else"))
                } else {
                    None
                };
                let bb_end = b.create_block("where_end");
                b.cond_branch(cond, bb_then, vec![], bb_else.unwrap_or(bb_end), vec![]);

                b.set_block(bb_then);
                lower_stmts(b, ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(bb_end, vec![]);
                }
                if let Some(bb_e) = bb_else {
                    b.set_block(bb_e);
                    if let Some((_m, else_body)) = elsewhere.first() {
                        lower_stmts(b, ctx, else_body);
                    }
                    if b.func().block(b.current_block()).terminator.is_none() {
                        b.branch(bb_end, vec![]);
                    }
                }
                b.set_block(bb_end);
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
                    let base = if info.allocatable {
                        b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                    } else {
                        info.addr
                    };
                    array_bases.insert(arr_name.clone(), base);
                }
            }

            let i_addr = b.alloca(IrType::Int(IntWidth::I64));
            let i_zero = b.const_i64(0);
            b.store(i_zero, i_addr);

            let bb_check = b.create_block("where_check");
            let bb_body = b.create_block("where_body");
            let bb_exit = b.create_block("where_exit");
            b.branch(bb_check, vec![]);

            b.set_block(bb_check);
            let i = b.load(i_addr);
            let done = b.icmp(CmpOp::Ge, i, n);
            b.cond_branch(done, bb_exit, vec![], bb_body, vec![]);

            b.set_block(bb_body);
            let i_val = b.load(i_addr);

            // Substitute each array variable with a scalar local bound to element i.
            // Save original locals for restoration.
            let mut saved_locals: Vec<(String, Option<LocalInfo>)> = Vec::new();
            for arr_name in &array_names {
                saved_locals.push((arr_name.clone(), ctx.locals.get(arr_name).cloned()));
                if let Some(orig_info) = ctx.locals.get(arr_name).cloned() {
                    let base = *array_bases.get(arr_name).unwrap();
                    // Compute element address: base + i * elem_bytes.
                    let elem_bytes_val = match &orig_info.ty {
                        IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => {
                            b.const_i64(8)
                        }
                        IrType::Int(IntWidth::I16) => b.const_i64(2),
                        IrType::Int(IntWidth::I8) => b.const_i64(1),
                        _ => b.const_i64(4),
                    };
                    let byte_off = b.imul(i_val, elem_bytes_val);
                    let elem_ptr = b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
                    // Replace the local with a scalar pointing to this element.
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

            // Pre-rewrite mask and body: any residual `name(section)`
            // FunctionCall AST node referencing a scalarized array name
            // would, after substitution, dispatch through the user-call
            // path on a scalar local and emit an undefined `bl _name` at
            // link time. Folding it to bare `Name` makes the substituted
            // per-iter scalar binding pick up at the element index.
            // Stdlib pattern: `where (lambda(1:m) > 0.0_sp) sv(1:m) =
            // sqrt(lambda(1:m) * real(n-1, sp))` — both `lambda(1:m)`
            // and `sv(1:m)` are scalarized to `lambda` / `sv` per iter.
            let rewritten_mask = rewrite_scalarized_section_refs(mask, &array_names);
            let rewritten_body: Vec<SpannedStmt> = body
                .iter()
                .map(|s| rewrite_scalarized_section_refs_stmt(s, &array_names))
                .collect();
            let rewritten_else: Vec<SpannedStmt> = elsewhere
                .first()
                .map(|(_m, els)| {
                    els.iter()
                        .map(|s| rewrite_scalarized_section_refs_stmt(s, &array_names))
                        .collect()
                })
                .unwrap_or_default();

            // Evaluate mask with element-level bindings.
            let cond = super::expr::lower_expr_ctx_tl(b, ctx, &rewritten_mask);

            let bb_then = b.create_block("where_then");
            let bb_else = b.create_block("where_else");
            let bb_incr = b.create_block("where_incr");
            b.cond_branch(cond, bb_then, vec![], bb_else, vec![]);

            b.set_block(bb_then);
            lower_stmts(b, ctx, &rewritten_body);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_incr, vec![]);
            }

            b.set_block(bb_else);
            if !rewritten_else.is_empty() {
                lower_stmts(b, ctx, &rewritten_else);
            }
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_incr, vec![]);
            }

            b.set_block(bb_incr);
            // Restore original locals.
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

        Stmt::WhereStmt { mask, stmt } => {
            // Single-line WHERE: where (cond) assignment.
            // F2018 §10.2.3.2: when the mask is an array-valued logical
            // expression, the assignment runs element-wise under the
            // mask. Reuse the WhereConstruct array-iteration shape: set
            // up per-element bindings for every array referenced in the
            // mask or assignment, evaluate the scalar mask, and run the
            // assignment under it.
            let mut array_names: Vec<String> = Vec::new();
            collect_array_names(mask, &ctx.locals, &mut array_names);
            collect_array_names_stmt(stmt, &ctx.locals, &mut array_names);

            if array_names.is_empty() {
                let cond = super::expr::lower_expr_ctx_tl(b, ctx, mask);
                let bb_then = b.create_block("where_stmt");
                let bb_end = b.create_block("where_stmt_end");
                b.cond_branch(cond, bb_then, vec![], bb_end, vec![]);
                b.set_block(bb_then);
                lower_stmt(b, ctx, stmt);
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.branch(bb_end, vec![]);
                }
                b.set_block(bb_end);
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
                    let base = if info.allocatable {
                        b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                    } else {
                        info.addr
                    };
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
                    let elem_bytes_val = match &orig_info.ty {
                        IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => {
                            b.const_i64(8)
                        }
                        IrType::Int(IntWidth::I16) => b.const_i64(2),
                        IrType::Int(IntWidth::I8) => b.const_i64(1),
                        _ => b.const_i64(4),
                    };
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
            let rewritten_mask = rewrite_scalarized_section_refs(mask, &array_names);
            let rewritten_stmt = rewrite_scalarized_section_refs_stmt(stmt, &array_names);

            let cond = super::expr::lower_expr_ctx_tl(b, ctx, &rewritten_mask);
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
        }

        Stmt::ForallConstruct {
            specs, mask, body, ..
        } => {
            // FORALL: nest loops. The body goes inside the innermost loop.
            // Build the body statements including optional mask as a closure-like pattern.
            // The innermost loop gets the real body; outer loops wrap it.
            lower_forall_nested(b, ctx, specs, mask.as_ref(), body);
        }

        Stmt::ForallStmt { specs, mask, stmt } => {
            let body_vec = vec![(**stmt).clone()];
            lower_forall_nested(b, ctx, specs, mask.as_ref(), &body_vec);
        }

        Stmt::SelectType {
            selector,
            guards,
            assoc_name,
            ..
        } => {
            let bb_end = b.create_block("select_type_end");
            let selector_type =
                operator_expr_type_info(selector, Some(&ctx.locals), ctx.st, Some(ctx.type_layouts));
            let selector_info = associate_alias_local_info(b, ctx, selector);
            let dynamic_class_selector = selector_info.as_ref().filter(|info| {
                info.derived_type.is_some()
                    && local_uses_array_descriptor(info)
                    && local_declared_rank(info) == 0
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
                let static_type = selector_info.as_ref().and_then(|info| info.derived_type.clone());
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
                                    let tag_matches = type_name.eq_ignore_ascii_case(guard_type);
                                    if tag_matches {
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

        Stmt::Stop { .. } => {
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
            );
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
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
            let typed_tbp_lookup =
                typed_allocate_tbp_lookup_value(b, type_spec.as_ref(), ctx.type_layouts);
            let typed_layout = typed_allocate_layout(type_spec.as_ref(), ctx.type_layouts);
            let source_desc = allocate_descriptor_keyword_expr(b, ctx, opts, "source");
            let mold_desc = allocate_descriptor_keyword_expr(b, ctx, opts, "mold");
            let shape_desc = source_desc.or(mold_desc);
            let source_expr = allocate_keyword_expr(opts, "source");
            let source_scalar_desc = allocate_scalar_source_descriptor(b, ctx, opts);

            for item in items {
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
                    if let Some((field_ptr, field)) = resolve_component_field_access(
                        b,
                        &ctx.locals,
                        component_expr,
                        ctx.st,
                        ctx.type_layouts,
                    ) {
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
                            init_allocated_string_descriptor(b, field_ptr, len_val);
                            if let Some((src_ptr, src_len)) = source_char {
                                b.call(
                                    FuncRef::External("afs_assign_char_deferred".into()),
                                    vec![field_ptr, src_ptr, src_len],
                                    IrType::Void,
                                );
                            }
                            continue;
                        }
                        if field.size == 384 && (field.allocatable || field.pointer) {
                            let elem_ty = field_storage_ir_type(&field, ctx.type_layouts);
                            let rank = args.len();
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
                                is_class: false,
            logical_kind: None,
                                last_dim_assumed_size: false,
                            };
                            let source_scalar_layout = if rank == 0 && source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_type_layout(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let source_scalar_type = if rank == 0 && source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_derived_type_name(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let dynamic_layout = source_scalar_layout
                                .or(typed_layout)
                                .or_else(|| {
                                    field_info
                                        .derived_type
                                        .as_deref()
                                        .and_then(|type_name| ctx.type_layouts.get(type_name))
                                });
                            let scalar_source_copy_plan =
                                if rank == 0 && source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_scalar_alloc_source_copy_plan(
                                        expr,
                                        &ctx.locals,
                                        ctx.st,
                                        ctx.type_layouts,
                                    )
                                })
                            } else {
                                None
                            };
                            let array_source_copy_layout = if source_desc.is_some() {
                                dynamic_layout.filter(|layout| {
                                    derived_layout_needs_deep_copy(layout, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let elem_size_bytes = dynamic_layout
                                .map(|layout| layout.size as i64)
                                .unwrap_or_else(|| descriptor_element_size_bytes(&field_info));
                            let es = allocated_array_elem_size(
                                b,
                                &field_info,
                                elem_size_bytes,
                                char_alloc_len,
                            );
                            let one_i64 = b.const_i64(1);
                            let dim_buf = if rank == 0 {
                                b.const_i64(0)
                            } else {
                                let dim_buf_bytes = (rank * 24) as u64;
                                let dim_buf = b.alloca(IrType::Array(
                                    Box::new(IrType::Int(IntWidth::I8)),
                                    dim_buf_bytes,
                                ));
                                for (i, arg) in args.iter().enumerate() {
                                    let (lo64, up64) = lower_alloc_bounds(b, ctx, &arg.value);
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
                                        vec![field_ptr, shape_desc, stat_addr],
                                        IrType::Void,
                                    );
                                } else {
                                    let rank_val = b.const_i32(0);
                                    b.call(
                                        FuncRef::External("afs_allocate_array".into()),
                                        vec![field_ptr, es, rank_val, dim_buf, stat_addr],
                                        IrType::Void,
                                    );
                                }
                            } else {
                                let rank_val = b.const_i32(rank as i32);
                                b.call(
                                    FuncRef::External("afs_allocate_array".into()),
                                    vec![field_ptr, es, rank_val, dim_buf, stat_addr],
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
                                    field_ptr,
                                    source_desc,
                                    rank > 0,
                                    array_source_copy_layout,
                                    scalar_source_copy_plan.as_ref(),
                                    ctx.type_layouts,
                                    errmsg_target.as_ref(),
                                );
                            } else if rank == 0 {
                                if let Some(source_desc) = source_scalar_desc {
                                    emit_allocatable_source_copy_on_success(
                                        b,
                                        stat_addr,
                                        field_ptr,
                                        source_desc,
                                        false,
                                        None,
                                        scalar_source_copy_plan.as_ref(),
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
                                        let dest_base = b.load_typed(
                                            field_ptr,
                                            IrType::Ptr(Box::new(elem_ty.clone())),
                                        );
                                        emit_scalar_allocate_source_init_on_success(
                                            b,
                                            ctx,
                                            stat_addr,
                                            dest_base,
                                            &elem_ty,
                                            source_scalar_type
                                                .as_deref()
                                                .or(field_derived_type_name(&field).as_deref()),
                                            source_expr,
                                        );
                                    }
                                }
                            }
                            if rank == 0 {
                                let field_type_name = field_derived_type_name(&field);
                                let type_tag = if source_desc.is_some() {
                                    None
                                } else if let Some(source_expr) = source_expr {
                                    expr_type_tag_value(
                                        b,
                                        source_expr,
                                        None,
                                        ctx.st,
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
                                let tbp_lookup = if source_desc.is_some() {
                                    None
                                } else if let Some(source_expr) = source_expr {
                                    expr_tbp_lookup_value(
                                        b,
                                        source_expr,
                                        None,
                                        ctx.st,
                                        ctx.type_layouts,
                                    )
                                    .or_else(|| {
                                        static_alloc_target_tbp_lookup_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                } else if let Some(ptr) = typed_tbp_lookup {
                                    Some(ptr)
                                } else {
                                    derived_type_tbp_lookup_value(
                                        b,
                                        field_type_name.as_deref(),
                                        ctx.type_layouts,
                                    )
                                    .or_else(|| {
                                        static_alloc_target_tbp_lookup_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                };
                                emit_scalar_alloc_polymorphic_metadata_on_success(
                                    b,
                                    stat_addr,
                                    field_ptr,
                                    type_tag,
                                    tbp_lookup,
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
                                }
                                let copied_from_source = source_desc.is_some()
                                    || source_scalar_desc.is_some()
                                    || source_expr.is_some();
                                if !copied_from_source {
                                    if let Some(layout) = dynamic_layout {
                                        let base_ptr = b.load_typed(
                                            field_ptr,
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                        if derived_layout_needs_runtime_initialization(
                                            layout,
                                            ctx.type_layouts,
                                        ) {
                                            initialize_derived_storage(
                                                b,
                                                base_ptr,
                                                layout,
                                                ctx.type_layouts,
                                            );
                                        }
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
                                local_storage_size_bytes(&field_info, ctx.type_layouts);
                            let size_val = b.const_i32(elem_size_bytes as i32);
                            let ptr = b.runtime_call(
                                RuntimeFunc::Allocate,
                                vec![size_val],
                                IrType::Ptr(Box::new(field_info.ty.clone())),
                            );
                            b.store(ptr, field_ptr);
                            if let Some(type_name) = &field_info.derived_type {
                                if let Some(layout) = ctx.type_layouts.get(type_name) {
                                    let base_ptr = b.load_typed(
                                        field_ptr,
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                    if derived_layout_needs_runtime_initialization(
                                        layout,
                                        ctx.type_layouts,
                                    ) {
                                        initialize_derived_storage(
                                            b,
                                            base_ptr,
                                            layout,
                                            ctx.type_layouts,
                                        );
                                    }
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
                            init_allocated_string_descriptor(b, desc, len_val);
                            if let Some((src_ptr, src_len)) = source_char {
                                b.call(
                                    FuncRef::External("afs_assign_char_deferred".into()),
                                    vec![desc, src_ptr, src_len],
                                    IrType::Void,
                                );
                            }
                            continue;
                        }
                        let elem_size_bytes = local_storage_size_bytes(&info, ctx.type_layouts);

                        if info.allocatable || info.descriptor_arg {
                            let rank = args.len();
                            let source_scalar_layout = if rank == 0 && source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_type_layout(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let source_scalar_type = if rank == 0 && source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_derived_type_name(expr, None, ctx.st, ctx.type_layouts)
                                })
                            } else {
                                None
                            };
                            let dynamic_layout = source_scalar_layout
                                .or(typed_layout)
                                .or_else(|| {
                                    info.derived_type
                                        .as_deref()
                                        .and_then(|type_name| ctx.type_layouts.get(type_name))
                                });
                            let scalar_source_copy_plan =
                                if rank == 0 && source_desc.is_none() {
                                source_expr.and_then(|expr| {
                                    expr_scalar_alloc_source_copy_plan(
                                        expr,
                                        &ctx.locals,
                                        ctx.st,
                                        ctx.type_layouts,
                                    )
                                })
                            } else {
                                None
                            };
                            let array_source_copy_layout = if source_desc.is_some() {
                                dynamic_layout.filter(|layout| {
                                    derived_layout_needs_deep_copy(layout, ctx.type_layouts)
                                })
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
                            let es = allocated_array_elem_size(
                                b,
                                &info,
                                dynamic_layout
                                    .map(|layout| layout.size as i64)
                                    .unwrap_or(elem_size_bytes),
                                char_alloc_len,
                            );
                            let desc = array_descriptor_addr(b, &info);
                            let one_i64 = b.const_i64(1);
                            let dim_buf = if rank == 0 {
                                b.const_i64(0)
                            } else {
                                let dim_buf_bytes = (rank * 24) as u64;
                                let dim_buf = b.alloca(IrType::Array(
                                    Box::new(IrType::Int(IntWidth::I8)),
                                    dim_buf_bytes,
                                ));
                                for (i, arg) in args.iter().enumerate() {
                                    let (lo64, up64) = lower_alloc_bounds(b, ctx, &arg.value);
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
                                        vec![desc, shape_desc, stat_addr],
                                        IrType::Void,
                                    );
                                } else {
                                    let rank_val = b.const_i32(0);
                                    b.call(
                                        FuncRef::External("afs_allocate_array".into()),
                                        vec![desc, es, rank_val, dim_buf, stat_addr],
                                        IrType::Void,
                                    );
                                }
                            } else {
                                let rank_val = b.const_i32(rank as i32);
                                b.call(
                                    FuncRef::External("afs_allocate_array".into()),
                                    vec![desc, es, rank_val, dim_buf, stat_addr],
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
                                    desc,
                                    source_desc,
                                    rank > 0,
                                    array_source_copy_layout,
                                    scalar_source_copy_plan.as_ref(),
                                    ctx.type_layouts,
                                    errmsg_target.as_ref(),
                                );
                            } else if rank == 0 {
                                if let Some(source_desc) = source_scalar_desc {
                                    emit_allocatable_source_copy_on_success(
                                        b,
                                        stat_addr,
                                        desc,
                                        source_desc,
                                        false,
                                        None,
                                        scalar_source_copy_plan.as_ref(),
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
                                        let dest_base = b.load_typed(
                                            desc,
                                            IrType::Ptr(Box::new(info.ty.clone())),
                                        );
                                        emit_scalar_allocate_source_init_on_success(
                                            b,
                                            ctx,
                                            stat_addr,
                                            dest_base,
                                            &info.ty,
                                            source_scalar_type
                                                .as_deref()
                                                .or(info.derived_type.as_deref()),
                                            source_expr,
                                        );
                                    }
                                }
                            }
                            if rank == 0 {
                                let type_tag = if source_desc.is_some() {
                                    None
                                } else if let Some(source_expr) = source_expr {
                                    expr_type_tag_value(
                                        b,
                                        source_expr,
                                        None,
                                        ctx.st,
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
                                let tbp_lookup = if source_desc.is_some() {
                                    None
                                } else if let Some(source_expr) = source_expr {
                                    expr_tbp_lookup_value(
                                        b,
                                        source_expr,
                                        None,
                                        ctx.st,
                                        ctx.type_layouts,
                                    )
                                    .or_else(|| {
                                        static_alloc_target_tbp_lookup_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                } else if let Some(ptr) = typed_tbp_lookup {
                                    Some(ptr)
                                } else {
                                    derived_type_tbp_lookup_value(
                                        b,
                                        info.derived_type.as_deref(),
                                        ctx.type_layouts,
                                    )
                                    .or_else(|| {
                                        static_alloc_target_tbp_lookup_value(
                                            b,
                                            item,
                                            ctx.st,
                                            ctx.type_layouts,
                                        )
                                    })
                                };
                                emit_scalar_alloc_polymorphic_metadata_on_success(
                                    b,
                                    stat_addr,
                                    desc,
                                    type_tag,
                                    tbp_lookup,
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
                                }
                                let copied_from_source = source_desc.is_some()
                                    || source_scalar_desc.is_some()
                                    || source_expr.is_some();
                                if !copied_from_source {
                                    if let Some(layout) = dynamic_layout {
                                        let base_ptr = b.load_typed(
                                            desc,
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                        if derived_layout_needs_runtime_initialization(
                                            layout,
                                            ctx.type_layouts,
                                        ) {
                                            initialize_derived_storage(
                                                b,
                                                base_ptr,
                                                layout,
                                                ctx.type_layouts,
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            // Non-allocatable array: old path (shouldn't happen for ALLOCATE).
                            let size_val = b.const_i32(elem_size_bytes as i32);
                            let ptr = b.runtime_call(
                                RuntimeFunc::Allocate,
                                vec![size_val],
                                IrType::Ptr(Box::new(info.ty.clone())),
                            );
                            let slot = if info.is_pointer && info.by_ref {
                                b.load_typed(
                                    info.addr,
                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                )
                            } else {
                                info.addr
                            };
                            b.store(ptr, slot);
                            if let Some(type_name) = &info.derived_type {
                                if let Some(layout) = ctx.type_layouts.get(type_name) {
                                    let base_ptr = b.load_typed(
                                        slot,
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                    if derived_layout_needs_runtime_initialization(
                                        layout,
                                        ctx.type_layouts,
                                    ) {
                                        initialize_derived_storage(
                                            b,
                                            base_ptr,
                                            layout,
                                            ctx.type_layouts,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
            super::core::emit_allocate_status_writeback(b, &stat_target);
        }

        Stmt::Deallocate { items, opts } => {
            let dealloc_stat_target = super::core::allocate_status_target(b, ctx, opts);
            let stat_addr = dealloc_stat_target.runtime_addr;
            let errmsg_target = allocate_errmsg_target(b, ctx, opts);
            for item in items {
                if let Expr::ComponentAccess { .. } = &item.node {
                    if let Some((field_ptr, field)) = resolve_component_field_access(
                        b,
                        &ctx.locals,
                        item,
                        ctx.st,
                        ctx.type_layouts,
                    ) {
                        if is_deferred_char_component_field(&field) {
                            b.call(
                                FuncRef::External("afs_dealloc_string".into()),
                                vec![field_ptr],
                                IrType::Void,
                            );
                            continue;
                        }
                        if field.size == 384 && (field.allocatable || field.pointer) {
                            b.call(
                                FuncRef::External("afs_deallocate_array".into()),
                                vec![field_ptr, stat_addr],
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
                            let ptr = b.load_typed(
                                field_ptr,
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            b.runtime_call(RuntimeFunc::Deallocate, vec![ptr], IrType::Void);
                            // F2018 §9.7.3.2: deallocating a pointer
                            // disassociates it.  Null the slot so a
                            // subsequent `associated()` returns false
                            // and `=> null()`-style sentinels work.
                            // Without this, free_map_entry_pool's
                            // `if (.not. associated(pool)) return`
                            // never fires for re-deallocated pools
                            // and recurses until stack overflow.
                            let null_v = b.const_i64(0);
                            let null_p = b.int_to_ptr(
                                null_v,
                                IrType::Int(IntWidth::I8),
                            );
                            b.store(null_p, field_ptr);
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
                                FuncRef::External("afs_dealloc_string".into()),
                                vec![desc],
                                IrType::Void,
                            );
                        } else if info.allocatable || info.descriptor_arg {
                            let desc = array_descriptor_addr(b, info);
                            b.call(
                                FuncRef::External("afs_deallocate_array".into()),
                                vec![desc, stat_addr],
                                IrType::Void,
                            );
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
                            let ptr = b
                                .load_typed(slot, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                            b.runtime_call(RuntimeFunc::Deallocate, vec![ptr], IrType::Void);
                            // Null the pointer slot per F2018 §9.7.3.2.
                            let null_v = b.const_i64(0);
                            let null_p = b.int_to_ptr(
                                null_v,
                                IrType::Int(IntWidth::I8),
                            );
                            b.store(null_p, slot);
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
            super::core::emit_allocate_status_writeback(b, &dealloc_stat_target);
        }

        Stmt::Block {
            uses,
            implicit,
            decls,
            body,
            ..
        } => {
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
            let saved: Vec<(String, Option<LocalInfo>)> = block_keys
                .iter()
                .map(|k| (k.clone(), ctx.locals.get(k).cloned()))
                .collect();
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
                    Some(ctx.type_layouts),
                );
            }
            if !uses.is_empty() {
                let required_import_names = collect_required_import_names(&effective_decls, body);
                install_globals_as_locals(
                    b,
                    &mut ctx.locals,
                    ctx.globals,
                    uses,
                    Some(&required_import_names),
                    None,
                    ctx.st,
                    &ctx.ambiguous_use_warnings,
                );
            }
            lower_stmts(b, ctx, body);
            // F2018 §7.5.6.3 / §9.7.3.2: at END BLOCK, finalize derived-type
            // locals that have FINAL subroutines and deallocate
            // block-scoped allocatables.  Only do this for keys that were
            // newly introduced by the block (not shadowed outer locals).
            if b.func().block(b.current_block()).terminator.is_none() {
                let block_only: HashMap<String, LocalInfo> = block_keys
                    .iter()
                    .filter(|k| ctx.locals.contains_key(*k))
                    .filter(|k| !saved.iter().any(|(sk, so)| sk == *k && so.is_some()))
                    .filter_map(|k| ctx.locals.get(k).map(|v| (k.clone(), v.clone())))
                    .collect();
                if !block_only.is_empty() {
                    insert_implicit_dealloc(
                        b,
                        &block_only,
                        &ctx.locals,
                        ctx.type_layouts,
                        ctx.st,
                        ctx.internal_funcs,
                        Some(ctx.contained_host_refs),
                        None,
                    );
                }
            }
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

        Stmt::Associate { assocs, body, .. } => {
            // Associate names are scoped — they only exist within the body.
            let added_keys: Vec<String> =
                assocs.iter().map(|(name, _)| name.to_lowercase()).collect();

            for (name, expr) in assocs {
                if let Some(info) = associate_alias_local_info(b, ctx, expr) {
                    ctx.locals.insert(name.to_lowercase(), info);
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
                    name.to_lowercase(),
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
            lower_stmts(b, ctx, body);

            // Remove associate names from scope.
            for key in &added_keys {
                ctx.locals.remove(key);
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

        Stmt::Goto { label } => {
            if let Some(&target_bb) = ctx.label_blocks.get(label) {
                b.branch(target_bb, vec![]);
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
                b.cond_branch(matches, target_bb, vec![], next_check, vec![]);
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
                .map(|s| {
                    lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &s.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    )
                })
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
                .map(|s| {
                    lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &s.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    )
                })
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
                .map(|s| {
                    lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &s.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    )
                })
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
                .map(|s| {
                    lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &s.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    )
                })
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
                .map(|s| {
                    lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &s.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    )
                })
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
            let unit_i32 = coerce_to_type(b, unit, &IrType::Int(IntWidth::I32));
            let recl_i64 = coerce_to_type(b, recl_val, &IrType::Int(IntWidth::I64));

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
            let has_iostat = iostat_spec.is_some();
            let has_newunit = newunit_spec.is_some();

            if !has_access && !has_form && !has_recl && !has_position && !has_iostat && !has_newunit
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
                    .map(|s| {
                        lower_string_expr_with_layouts(
                            b,
                            &ctx.locals,
                            &s.value,
                            ctx.st,
                            Some(ctx.type_layouts),
                        )
                    })
                    .unwrap_or_else(|| {
                        let z = b.const_i64(0);
                        (z, z)
                    });

                // Layout matches repr(C) OpenControlBlock (128 bytes):
                //   0: unit(i32) + 4 pad, 8: filename(ptr), 16: filename_len(i64),
                //  24: status(ptr), 32: status_len(i64), 40: action(ptr), 48: action_len(i64),
                //  56: access(ptr), 64: access_len(i64), 72: form(ptr), 80: form_len(i64),
                //  88: recl(i64), 96: iostat(ptr), 104: newunit(ptr),
                // 112: position(ptr), 120: position_len(i64)
                let cb_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 128);
                let cb = b.alloca(cb_ty);

                let store_at = |b: &mut crate::ir::builder::FuncBuilder,
                                base,
                                offset: i64,
                                field_ty: IrType,
                                val| {
                    let field_bytes = field_ty.size_bytes() as i64;
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
                let iostat_ptr = iostat_spec
                    .map(|spec| lower_arg_by_ref_ctx(b, ctx, &spec.value))
                    .unwrap_or(null);
                let newunit_ptr = newunit_spec
                    .map(|spec| lower_arg_by_ref_ctx(b, ctx, &spec.value))
                    .unwrap_or(null);
                let iostat_ptr_ty = b
                    .func()
                    .value_type(iostat_ptr)
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
                store_at(b, cb, 96, iostat_ptr_ty, iostat_ptr);
                store_at(b, cb, 104, newunit_ptr_ty, newunit_ptr);
                store_at(b, cb, 112, position_ptr_ty, position_ptr);
                store_at(b, cb, 120, IrType::Int(IntWidth::I64), position_len);

                b.call(FuncRef::External("afs_open".into()), vec![cb], IrType::Void);
            }
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
                .map(|spec| {
                    lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &spec.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    )
                })
                .unwrap_or_else(|| (null, null));
            b.call(
                FuncRef::External("afs_close_ex".into()),
                vec![unit_i32, status_ptr, status_len, iostat_ptr],
                IrType::Void,
            );
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
                    let (p, l) = lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &c.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    );
                    Some(b.call(
                        FuncRef::External("afs_advance_eval".into()),
                        vec![p, l],
                        IrType::Int(IntWidth::I32),
                    ))
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

            let explicit_iostat_addr = controls
                .iter()
                .find(|c| {
                    c.keyword
                        .as_deref()
                        .map(|k| k.eq_ignore_ascii_case("iostat"))
                        .unwrap_or(false)
                })
                .map(|c| lower_arg_by_ref_ctx(b, ctx, &c.value));

            let iostat_addr = match (err_label, explicit_iostat_addr) {
                (_, Some(addr)) => addr,
                (Some(_), None) => {
                    let tmp = b.alloca(IrType::Int(IntWidth::I32));
                    let zero = b.const_i32(0);
                    b.store(zero, tmp);
                    tmp
                }
                (None, None) => b.const_i64(0),
            };

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

            if let Some(ctrl) = controls.first() {
                if let Some((buf_ptr, buf_len)) = internal_io_buffer(b, ctx, ctrl) {
                    if is_list_directed {
                        lower_internal_read_items(b, ctx, items, buf_ptr, buf_len, iostat_addr);
                    } else {
                        let (fmt_ptr, fmt_len) = lower_string_expr_with_layouts(
                            b,
                            &ctx.locals,
                            &fmt_control.unwrap().value,
                            ctx.st,
                            Some(ctx.type_layouts),
                        );
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
                    }
                    lower_read_err_branch(b, ctx, err_label, iostat_addr);
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
            if is_list_directed {
                // Wrap the per-item reads in begin/end so the runtime
                // can slurp a sequential-unformatted record up front
                // and let the typed helpers consume binary bytes.
                // Formatted units pass through (begin only resets
                // iostat). iomsg= isn't yet plumbed on the read side;
                // stick a null pointer for now.
                let null_iomsg = {
                    let z = b.const_i64(0);
                    b.int_to_ptr(z, IrType::Int(IntWidth::I8))
                };
                let zero_len = b.const_i64(0);
                b.call(
                    FuncRef::External("afs_list_read_begin".into()),
                    vec![unit, iostat_addr, null_iomsg, zero_len],
                    IrType::Void,
                );
                lower_list_read_items(b, ctx, items, unit, iostat_addr);
                b.call(
                    FuncRef::External("afs_list_read_end".into()),
                    vec![unit, iostat_addr, null_iomsg, zero_len],
                    IrType::Void,
                );
            } else {
                let (fmt_ptr, fmt_len) = lower_string_expr_with_layouts(
                    b,
                    &ctx.locals,
                    &fmt_control.unwrap().value,
                    ctx.st,
                    Some(ctx.type_layouts),
                );
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
            }
            lower_read_err_branch(b, ctx, err_label, iostat_addr);
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

            let lower_ref_spec = |b: &mut FuncBuilder, needle: &str| -> ValueId {
                spec_by_keyword(needle)
                    .map(|spec| lower_arg_by_ref_ctx(b, ctx, &spec.value))
                    .unwrap_or(null)
            };
            let lower_string_spec = |b: &mut FuncBuilder, needle: &str| -> (ValueId, ValueId) {
                if let Some(spec) = spec_by_keyword(needle) {
                    lower_string_expr_with_layouts(
                        b,
                        &ctx.locals,
                        &spec.value,
                        ctx.st,
                        Some(ctx.type_layouts),
                    )
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
            let recl_addr = lower_ref_spec(b, "recl");
            let size_spec = spec_by_keyword("size");
            let (size_addr, size_storeback) = if let Some(spec) = size_spec {
                let dest_addr = lower_arg_by_ref_ctx(b, ctx, &spec.value);
                let temp = b.alloca(IrType::Int(IntWidth::I64));
                (temp, Some(dest_addr))
            } else {
                (null, None)
            };

            if let Some(fs) = file_spec {
                let (fptr, flen) = lower_string_expr_with_layouts(
                    b,
                    &ctx.locals,
                    &fs.value,
                    ctx.st,
                    Some(ctx.type_layouts),
                );
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
                        read_ptr,
                        read_len,
                        write_ptr,
                        write_len,
                        readwrite_ptr,
                        readwrite_len,
                    ],
                    IrType::Void,
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
                        read_ptr,
                        read_len,
                        write_ptr,
                        write_len,
                        readwrite_ptr,
                        readwrite_len,
                    ],
                    IrType::Void,
                );
            }
            if let Some(dest_addr) = size_storeback {
                let size_val = b.load(size_addr);
                let dest_ty = match b.func().value_type(dest_addr) {
                    Some(IrType::Ptr(inner)) => (*inner).clone(),
                    _ => IrType::Int(IntWidth::I32),
                };
                let coerced = coerce_to_type(b, size_val, &dest_ty);
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
                // Array pointers use the 384-byte descriptor, scalar
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
            //   * array pointer: slot holds a 384-byte ArrayDescriptor,
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
                        .map(|info| {
                            info.is_pointer && local_uses_array_descriptor(info)
                        })
                        .unwrap_or(false);
                    let all_ranges = !args.is_empty()
                        && args.iter().all(|a| {
                            matches!(
                                a.value,
                                crate::ast::expr::SectionSubscript::Range { .. }
                            )
                        });
                    if is_remap_target
                        && all_ranges
                        && lower_rank_remap_pointer_assignment(
                            b, ctx, &tgt_key, args, value,
                        )
                    {
                        return;
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
                                let mut closure_args = Vec::new();
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
                                            store_string_descriptor_view(
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
                            store_string_descriptor_view(b, *tgt_field_ptr, ptr, len);
                            return;
                        }
                    }
                    let (ptr, len) = lower_string_expr_ctx(b, ctx, value);
                    store_string_descriptor_view(b, *tgt_field_ptr, ptr, len);
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
                        && tgt_field.size != 384
                    {
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))
                    } else {
                        field_storage_ir_type(&tgt_field, ctx.type_layouts)
                    },
                    dims: vec![],
                    allocatable: tgt_field.size == 384
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
                        store_string_descriptor_view(b, tgt_desc, ptr, len);
                        return;
                    }
                }
                if let Expr::Name { name: src_name } = &value.node {
                    if let Some(src_info) = ctx.locals.get(&src_name.to_lowercase()) {
                        if matches!(src_info.char_kind, CharKind::Deferred) {
                            let src_desc = string_descriptor_addr(b, src_info);
                            let (ptr, len) = load_string_descriptor_view(b, src_desc);
                            store_string_descriptor_view(b, tgt_desc, ptr, len);
                            return;
                        }
                    }
                }
                let (ptr, len) = lower_string_expr_with_layouts(
                    b,
                    &ctx.locals,
                    value,
                    ctx.st,
                    Some(ctx.type_layouts),
                );
                store_string_descriptor_view(b, tgt_desc, ptr, len);
                return;
            }

            // Handle section-RHS: pa => ia(lo:hi).  The RHS is a
            // FunctionCall{Name(arr), [Range(lo,hi)]}.  Build a
            // descriptor pointing at arr(lo) with extent hi-lo+1.
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
                        {
                            if let crate::ast::expr::SectionSubscript::Range {
                                start,
                                end,
                                stride: _,
                            } = &val_args[0].value
                            {
                                let base = array_data_ptr_for_call(b, &arr_info);
                                let lo = if let Some(se) = start {
                                    let v = super::expr::lower_expr_ctx(b, ctx, se);
                                    match b.func().value_type(v) {
                                        Some(IrType::Int(IntWidth::I64)) => v,
                                        _ => b.int_extend(v, IntWidth::I64, true),
                                    }
                                } else {
                                    b.const_i64(1)
                                };
                                let hi = if let Some(ee) = end {
                                    let v = super::expr::lower_expr_ctx(b, ctx, ee);
                                    match b.func().value_type(v) {
                                        Some(IrType::Int(IntWidth::I64)) => v,
                                        _ => b.int_extend(v, IntWidth::I64, true),
                                    }
                                } else {
                                    array_total_elems_value(b, &arr_info)
                                };
                                // Build a descriptor in the pointer's slot.
                                let desc = array_descriptor_addr(b, &tgt_info);
                                let zero32 = b.const_i32(0);
                                let sz384 = b.const_i64(384);
                                b.call(
                                    FuncRef::External("memset".into()),
                                    vec![desc, zero32, sz384],
                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                );
                                // base_addr: base + (lo - 1) * elem_size
                                let one = b.const_i64(1);
                                let lo_0 = b.isub(lo, one);
                                let elem_bytes = b.const_i64(ir_scalar_byte_size(&arr_info.ty));
                                let byte_off = b.imul(lo_0, elem_bytes);
                                let slice_base =
                                    b.gep(base, vec![byte_off], IrType::Int(IntWidth::I8));
                                store_byte_aggregate_field(
                                    b,
                                    desc,
                                    0,
                                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    slice_base,
                                );
                                store_byte_aggregate_field(
                                    b,
                                    desc,
                                    8,
                                    IrType::Int(IntWidth::I64),
                                    elem_bytes,
                                );
                                let rank = b.const_i32(1);
                                store_byte_aggregate_field(
                                    b,
                                    desc,
                                    16,
                                    IrType::Int(IntWidth::I32),
                                    rank,
                                );
                                let flags = b.const_i32(2);
                                store_byte_aggregate_field(
                                    b,
                                    desc,
                                    20,
                                    IrType::Int(IntWidth::I32),
                                    flags,
                                );
                                // dim[0]: lower=1, upper=extent, stride=1
                                store_byte_aggregate_field(
                                    b,
                                    desc,
                                    24,
                                    IrType::Int(IntWidth::I64),
                                    one,
                                );
                                let extent = b.isub(hi, lo);
                                let extent1 = b.iadd(extent, one);
                                store_byte_aggregate_field(
                                    b,
                                    desc,
                                    32,
                                    IrType::Int(IntWidth::I64),
                                    extent1,
                                );
                                store_byte_aggregate_field(
                                    b,
                                    desc,
                                    40,
                                    IrType::Int(IntWidth::I64),
                                    one,
                                );
                                return;
                            }
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
                            if field.size == 384 && (field.pointer || field.allocatable) {
                                if local_uses_array_descriptor(&tgt_info) {
                                    let tgt_desc = array_descriptor_addr(b, &tgt_info);
                                    let size = b.const_i64(384);
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
                    let tbp_lookup =
                        static_expr_tbp_lookup_value(b, value, ctx.st, ctx.type_layouts);
                    let tgt_desc = array_descriptor_addr(b, &tgt_info);
                    store_scalar_polymorphic_descriptor_view(
                        b,
                        tgt_desc,
                        addr,
                        elem_size,
                        type_tag,
                        tbp_lookup,
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
            // target and memcpy 384 bytes into the pointer's slot.
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
                    let size = b.const_i64(384);
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
                let tbp_lookup =
                    static_expr_tbp_lookup_value(b, value, ctx.st, ctx.type_layouts);
                let tgt_desc = array_descriptor_addr(b, &tgt_info);
                store_scalar_polymorphic_descriptor_view(
                    b,
                    tgt_desc,
                    addr,
                    elem_size,
                    type_tag,
                    tbp_lookup,
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
