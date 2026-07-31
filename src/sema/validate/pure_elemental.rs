//! Pure / elemental procedure constraint checks.
//!
//! Extracted from `core.rs` in Sprint 13: F2018 §15.7 says a PURE
//! procedure may only call PURE/ELEMENTAL/intrinsic procedures, may
//! not define variables visible by host- or USE-association, and an
//! ELEMENTAL procedure's dummy args must be scalar. This module is
//! the home for those checks.

use crate::ast::decl::{Attribute, Decl};
use crate::ast::expr::{AcValue, Expr, SectionSubscript, SpannedExpr};
use crate::ast::stmt::{CaseSelector, SpannedStmt, Stmt};
use crate::ast::unit::DummyArg;
use crate::lexer::Span;
use crate::sema::symtab::{ScopeId, Symbol, SymbolKind, SymbolTable};

use super::core::{extract_base_name, Ctx};

fn check_pure_subscript_calls(ctx: &mut Ctx, subscript: &SectionSubscript) {
    match subscript {
        SectionSubscript::Element(expr) => check_pure_expr_calls(ctx, expr),
        SectionSubscript::Range { start, end, stride } => {
            for expr in [start, end, stride].into_iter().flatten() {
                check_pure_expr_calls(ctx, expr);
            }
        }
    }
}

fn check_pure_array_constructor_calls(ctx: &mut Ctx, value: &AcValue) {
    match value {
        AcValue::Expr(expr) => check_pure_expr_calls(ctx, expr),
        AcValue::ImpliedDo(implied_do) => {
            for value in &implied_do.values {
                check_pure_array_constructor_calls(ctx, value);
            }
            check_pure_expr_calls(ctx, &implied_do.start);
            check_pure_expr_calls(ctx, &implied_do.end);
            if let Some(step) = &implied_do.step {
                check_pure_expr_calls(ctx, step);
            }
        }
    }
}

pub(super) fn check_pure_expr_calls(ctx: &mut Ctx, expr: &SpannedExpr) {
    match &expr.node {
        Expr::FunctionCall { callee, args } => {
            validate_pure_call(ctx, callee, expr.span);
            check_pure_expr_calls(ctx, callee);
            for arg in args {
                check_pure_subscript_calls(ctx, &arg.value);
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            check_pure_expr_calls(ctx, left);
            check_pure_expr_calls(ctx, right);
        }
        Expr::UnaryOp { operand, .. } => check_pure_expr_calls(ctx, operand),
        Expr::ParenExpr { inner } => check_pure_expr_calls(ctx, inner),
        Expr::ComplexLiteral { real, imag } => {
            check_pure_expr_calls(ctx, real);
            check_pure_expr_calls(ctx, imag);
        }
        Expr::ComponentAccess { base, .. } => check_pure_expr_calls(ctx, base),
        Expr::ArrayConstructor { values, .. } => {
            for value in values {
                check_pure_array_constructor_calls(ctx, value);
            }
        }
        Expr::ConditionalExpr {
            cond,
            then_val,
            else_val,
        } => {
            check_pure_expr_calls(ctx, cond);
            check_pure_expr_calls(ctx, then_val);
            check_pure_expr_calls(ctx, else_val);
        }
        Expr::IntegerLiteral { .. }
        | Expr::RealLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::BozLiteral { .. }
        | Expr::Name { .. }
        | Expr::NilArgument => {}
    }
}

/// Check every expression owned directly by a statement. Nested statement
/// bodies are intentionally left to `validate_stmt`, which establishes the
/// correct BLOCK/ASSOCIATE/SELECT refinement context before recursing.
pub(super) fn check_pure_stmt_expr_calls(ctx: &mut Ctx, stmt: &SpannedStmt) {
    let check_controls = |ctx: &mut Ctx, controls: &[crate::ast::stmt::IoControl]| {
        for control in controls {
            check_pure_expr_calls(ctx, &control.value);
        }
    };
    let check_args = |ctx: &mut Ctx, args: &[crate::ast::expr::Argument]| {
        for arg in args {
            check_pure_subscript_calls(ctx, &arg.value);
        }
    };

    match &stmt.node {
        Stmt::Assignment { target, value } | Stmt::PointerAssignment { target, value } => {
            check_pure_expr_calls(ctx, target);
            check_pure_expr_calls(ctx, value);
        }
        Stmt::IfConstruct {
            condition,
            else_ifs,
            ..
        } => {
            check_pure_expr_calls(ctx, condition);
            for (condition, _) in else_ifs {
                check_pure_expr_calls(ctx, condition);
            }
        }
        Stmt::IfStmt { condition, .. }
        | Stmt::DoWhile { condition, .. }
        | Stmt::SelectType {
            selector: condition,
            ..
        }
        | Stmt::SelectRank {
            selector: condition,
            ..
        }
        | Stmt::ComputedGoto {
            selector: condition,
            ..
        }
        | Stmt::ArithmeticIf {
            expr: condition, ..
        }
        | Stmt::WhereStmt {
            mask: condition, ..
        } => check_pure_expr_calls(ctx, condition),
        Stmt::DoLoop {
            start, end, step, ..
        } => {
            for expr in [start, end, step].into_iter().flatten() {
                check_pure_expr_calls(ctx, expr);
            }
        }
        Stmt::DoConcurrent { controls, mask, .. } => {
            for control in controls {
                check_pure_expr_calls(ctx, &control.start);
                check_pure_expr_calls(ctx, &control.end);
                if let Some(step) = &control.step {
                    check_pure_expr_calls(ctx, step);
                }
            }
            if let Some(mask) = mask {
                check_pure_expr_calls(ctx, mask);
            }
        }
        Stmt::SelectCase {
            selector, cases, ..
        } => {
            check_pure_expr_calls(ctx, selector);
            for case in cases {
                for selector in &case.selectors {
                    match selector {
                        CaseSelector::Value(expr) => check_pure_expr_calls(ctx, expr),
                        CaseSelector::Range { low, high } => {
                            for expr in [low, high].into_iter().flatten() {
                                check_pure_expr_calls(ctx, expr);
                            }
                        }
                        CaseSelector::Default => {}
                    }
                }
            }
        }
        Stmt::WhereConstruct {
            mask, elsewhere, ..
        } => {
            check_pure_expr_calls(ctx, mask);
            for (mask, _) in elsewhere {
                if let Some(mask) = mask {
                    check_pure_expr_calls(ctx, mask);
                }
            }
        }
        Stmt::ForallConstruct { specs, mask, .. } | Stmt::ForallStmt { specs, mask, .. } => {
            for spec in specs {
                check_pure_expr_calls(ctx, &spec.start);
                check_pure_expr_calls(ctx, &spec.end);
                if let Some(step) = &spec.step {
                    check_pure_expr_calls(ctx, step);
                }
            }
            if let Some(mask) = mask {
                check_pure_expr_calls(ctx, mask);
            }
        }
        Stmt::Associate { assocs, .. } => {
            for (_, selector) in assocs {
                check_pure_expr_calls(ctx, selector);
            }
        }
        Stmt::Stop { code, quiet } | Stmt::ErrorStop { code, quiet } => {
            for expr in [code, quiet].into_iter().flatten() {
                check_pure_expr_calls(ctx, expr);
            }
        }
        Stmt::Return { value } => {
            if let Some(value) = value {
                check_pure_expr_calls(ctx, value);
            }
        }
        Stmt::Write { controls, items }
        | Stmt::Read { controls, items }
        | Stmt::Inquire {
            specs: controls,
            items,
        } => {
            check_controls(ctx, controls);
            for item in items {
                check_pure_expr_calls(ctx, item);
            }
        }
        Stmt::Open { specs }
        | Stmt::Close { specs }
        | Stmt::Rewind { specs }
        | Stmt::Backspace { specs }
        | Stmt::Endfile { specs }
        | Stmt::Flush { specs }
        | Stmt::Wait { specs } => check_controls(ctx, specs),
        Stmt::Allocate { items, opts, .. } | Stmt::Deallocate { items, opts } => {
            for item in items {
                check_pure_expr_calls(ctx, item);
            }
            check_controls(ctx, opts);
        }
        Stmt::Nullify { items } => {
            for item in items {
                check_pure_expr_calls(ctx, item);
            }
        }
        Stmt::Call { callee, args } => {
            check_pure_expr_calls(ctx, callee);
            check_args(ctx, args);
        }
        Stmt::Print { format, items } => {
            check_pure_expr_calls(ctx, format);
            for item in items {
                check_pure_expr_calls(ctx, item);
            }
        }
        Stmt::Block { .. }
        | Stmt::Declaration(_)
        | Stmt::Exit { .. }
        | Stmt::Cycle { .. }
        | Stmt::Goto { .. }
        | Stmt::Labeled { .. }
        | Stmt::Continue { .. }
        | Stmt::Format { .. }
        | Stmt::Namelist { .. } => {}
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
    if ctx.is_block_local_name(&name) {
        return;
    }
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
