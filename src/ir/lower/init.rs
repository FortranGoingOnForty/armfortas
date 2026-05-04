//! Initializer lowering for declared variables.
//!
//! Extracted from `core.rs` in Sprint 11 Stage E. Pure mechanical
//! move — behavior unchanged.

use std::collections::HashMap;

use crate::ast::decl::Decl;
use crate::ast::expr::Expr;
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;
use crate::sema::symtab::SymbolTable;

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
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
) {
    // Pre-collect the set of GlobalAddr-defining ValueIds so the
    // inner skip check is O(1). Audit Maj-3.
    let global_addr_ids = collect_global_addr_values(b);
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
