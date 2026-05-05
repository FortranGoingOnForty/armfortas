//! Pure / elemental procedure constraint checks.
//!
//! Extracted from `core.rs` in Sprint 13: F2018 §15.7 says a PURE
//! procedure may only call PURE/ELEMENTAL/intrinsic procedures, may
//! not define variables visible by host- or USE-association, and an
//! ELEMENTAL procedure's dummy args must be scalar. This module is
//! the home for those checks.

use crate::ast::decl::{Attribute, Decl};
use crate::ast::expr::Expr;
use crate::ast::unit::DummyArg;
use crate::lexer::Span;
use crate::sema::symtab::{ScopeId, Symbol, SymbolKind, SymbolTable};

use super::core::{extract_base_name, Ctx};

pub(super) fn check_pure_expr_calls(ctx: &mut Ctx, expr: &crate::ast::expr::SpannedExpr) {
    match &expr.node {
        Expr::FunctionCall { callee, args } => {
            validate_pure_call(ctx, callee, expr.span);
            for arg in args {
                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                    check_pure_expr_calls(ctx, e);
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            check_pure_expr_calls(ctx, left);
            check_pure_expr_calls(ctx, right);
        }
        Expr::UnaryOp { operand, .. } => check_pure_expr_calls(ctx, operand),
        Expr::ParenExpr { inner } => check_pure_expr_calls(ctx, inner),
        _ => {}
    }
}

pub(super) fn validate_pure_call(
    ctx: &mut Ctx,
    callee: &crate::ast::expr::SpannedExpr,
    span: Span,
) {
    // F2018 15.7: a PURE procedure may only call PURE, ELEMENTAL,
    // or intrinsic procedures.  If the callee resolves to a known
    // symbol that is NOT marked pure/elemental/intrinsic, reject.
    // Unknown callees (external without an interface) are left
    // alone — the programmer's responsibility per F2018 §15.4.
    let Some(name) = extract_base_name(callee) else {
        return;
    };
    let Some(sym) = ctx.lookup(&name) else {
        return;
    };
    match sym.kind {
        SymbolKind::Function | SymbolKind::Subroutine
            if !sym.attrs.pure && !sym.attrs.elemental && !sym.attrs.intrinsic =>
        {
            ctx.error(
                span,
                format!(
                    "call to '{}' inside a pure procedure: callee is not pure, elemental, or intrinsic (F2018 15.7)",
                    sym.name
                ),
            );
        }
        SymbolKind::IntrinsicProc => {} // always OK
        _ => {}                         // external / unknown — can't check
    }
}

/// True if `sym` is declared outside the procedure rooted at
/// `procedure_scope` — i.e. it comes from host association, USE
/// association, or a COMMON block in an enclosing unit.  This is
/// the F2018 15.7 "accessed by host or use association, or in
/// common" predicate that makes a variable off-limits for
/// definition inside a PURE procedure body.
pub(super) fn symbol_is_non_local_to_procedure(
    st: &SymbolTable,
    sym: &Symbol,
    procedure_scope: ScopeId,
) -> bool {
    // Walk from `sym.scope` up the parent chain.  If we reach
    // `procedure_scope` (or a descendant we started from), the
    // symbol lives inside the current procedure — that's OK.
    // If we reach the top (Global) without crossing the procedure
    // boundary, the symbol is in an enclosing scope (module,
    // parent program, parent subroutine).
    let mut cur = Some(sym.scope);
    while let Some(sid) = cur {
        if sid == procedure_scope {
            return false;
        }
        cur = st.scope(sid).parent;
    }
    true
}

/// Reject a PURE-procedure statement that would define a variable
/// visible via host/use association or a common block.  The
/// caller supplies the designator's root name; we look it up in
/// the current scope and check whether its home scope lies
/// outside the enclosing procedure.  F2018 15.7, C1598.
pub(super) fn reject_pure_nonlocal_definition(
    ctx: &mut Ctx,
    target: &crate::ast::expr::SpannedExpr,
    span: Span,
    stmt_label: &str,
) {
    if !ctx.in_pure {
        return;
    }
    let Some(name) = extract_base_name(target) else {
        return;
    };
    let Some(sym) = ctx.lookup(&name) else {
        return;
    };
    // Only variables and COMMON blocks can be "defined"; function
    // names get definition semantics too but those are the pure
    // function's own result variable (always local).
    if !matches!(
        sym.kind,
        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::CommonBlock
    ) {
        return;
    }
    if symbol_is_non_local_to_procedure(ctx.st, sym, ctx.scope_id) {
        let sym_name = sym.name.clone();
        ctx.error(
            span,
            format!(
                "{} target '{}' is accessed by host or use association and cannot be defined inside a pure procedure (F2018 15.7)",
                stmt_label, sym_name
            ),
        );
    }
}

/// Validate elemental procedure arguments are scalar.
pub(super) fn validate_elemental_args(
    ctx: &mut Ctx,
    args: &[DummyArg],
    decls: &[crate::ast::decl::SpannedDecl],
    span: Span,
) {
    for arg in args {
        if let DummyArg::Name(arg_name) = arg {
            for decl in decls {
                if let Decl::TypeDecl {
                    attrs, entities, ..
                } = &decl.node
                {
                    for entity in entities {
                        if entity.name.eq_ignore_ascii_case(arg_name) {
                            let has_dimension =
                                attrs.iter().any(|a| matches!(a, Attribute::Dimension(_)));
                            let has_entity_dims = entity.array_spec.is_some();
                            if has_dimension || has_entity_dims {
                                ctx.error(
                                    span,
                                    format!(
                                        "elemental procedure argument '{}' must be scalar",
                                        arg_name
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
