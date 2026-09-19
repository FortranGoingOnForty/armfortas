//! Semantic validation for executable OpenMP constructs.
//!
//! The first executable slice is intentionally narrow: a `PARALLEL` region
//! may contain code that needs no captured Fortran data and may use only the
//! `IF` and `NUM_THREADS` clauses. Keeping that boundary explicit lets the
//! outliner execute real concurrent regions without pretending that the data
//! environment is already implemented.

use std::collections::HashSet;

use crate::ast::openmp::{OpenMpClause, OpenMpConstruct};
use crate::ast::stmt::{IoControl, RankGuard, SpannedStmt, Stmt, TypeGuard};
use crate::lexer::Span;
use crate::sema::symtab::TypeInfo;

use super::core::{
    collect_default_none_nested_block_references, collect_reference_stmts, validation_expr_rank,
    validation_expr_type_info, Ctx, ProcedureReferenceFacts, ReferenceRole,
};

pub(super) fn validate_construct(ctx: &mut Ctx<'_>, span: Span, construct: &OpenMpConstruct) {
    let OpenMpConstruct::Parallel { clauses, body } = construct else {
        ctx.error(
            span,
            format!(
                "OpenMP {} execution is recognized but not yet implemented",
                construct.name()
            ),
        );
        return;
    };

    if ctx.in_pure {
        ctx.error(span, "OpenMP PARALLEL is not allowed in a PURE procedure");
    }

    validate_parallel_clauses(ctx, span, clauses);
    validate_capture_free_body(ctx, body);
    validate_structured_block(ctx, body);
}

fn validate_parallel_clauses(ctx: &mut Ctx<'_>, span: Span, clauses: &[OpenMpClause]) {
    let mut saw_if = false;
    let mut saw_num_threads = false;

    for clause in clauses {
        match clause {
            OpenMpClause::If {
                modifier,
                condition,
            } => {
                if std::mem::replace(&mut saw_if, true) {
                    ctx.error(span, "OpenMP PARALLEL may not repeat the IF clause");
                }
                if modifier
                    .as_deref()
                    .is_some_and(|modifier| !modifier.eq_ignore_ascii_case("parallel"))
                {
                    ctx.error(
                        condition.span,
                        "OpenMP PARALLEL IF clause modifier must be PARALLEL",
                    );
                }
                if validation_expr_rank(ctx, condition) != Some(0)
                    || !matches!(
                        validation_expr_type_info(ctx, condition),
                        Some(TypeInfo::Logical { .. })
                    )
                {
                    ctx.error(
                        condition.span,
                        "OpenMP PARALLEL IF condition must be a scalar LOGICAL expression",
                    );
                }
            }
            OpenMpClause::NumThreads(count) => {
                if std::mem::replace(&mut saw_num_threads, true) {
                    ctx.error(
                        span,
                        "OpenMP PARALLEL may not repeat the NUM_THREADS clause",
                    );
                }
                if validation_expr_rank(ctx, count) != Some(0)
                    || !matches!(
                        validation_expr_type_info(ctx, count),
                        Some(TypeInfo::Integer { .. })
                    )
                {
                    ctx.error(
                        count.span,
                        "OpenMP PARALLEL NUM_THREADS expression must be a scalar INTEGER",
                    );
                }
            }
            unsupported => ctx.error(
                span,
                format!(
                    "OpenMP {} clause on PARALLEL is recognized but not yet implemented",
                    clause_name(unsupported)
                ),
            ),
        }
    }
}

fn clause_name(clause: &OpenMpClause) -> &'static str {
    match clause {
        OpenMpClause::Private(_) => "PRIVATE",
        OpenMpClause::FirstPrivate(_) => "FIRSTPRIVATE",
        OpenMpClause::Shared(_) => "SHARED",
        OpenMpClause::Default(_) => "DEFAULT",
        OpenMpClause::If { .. } => "IF",
        OpenMpClause::NumThreads(_) => "NUM_THREADS",
        OpenMpClause::Schedule { .. } => "SCHEDULE",
        OpenMpClause::Collapse(_) => "COLLAPSE",
        OpenMpClause::Nowait => "NOWAIT",
        OpenMpClause::Reduction { .. } => "REDUCTION",
    }
}

fn validate_capture_free_body(ctx: &mut Ctx<'_>, body: &[SpannedStmt]) {
    let shadowed = HashSet::new();
    let mut facts = ProcedureReferenceFacts::default();
    collect_reference_stmts(body, &shadowed, &mut facts);
    collect_default_none_nested_block_references(ctx.st, body, &shadowed, &mut facts);

    let mut reported = HashSet::new();
    for reference in facts.references {
        if reference.role == ReferenceRole::Value && reported.insert(reference.name.clone()) {
            ctx.error(
                reference.span,
                format!(
                    "OpenMP PARALLEL data reference '{}' requires data-environment capture support, which is not yet implemented",
                    reference.name
                ),
            );
        }
    }
}

fn validate_structured_block(ctx: &mut Ctx<'_>, stmts: &[SpannedStmt]) {
    for stmt in stmts {
        match &stmt.node {
            Stmt::Return { .. } => reject_transfer(ctx, stmt.span, "RETURN"),
            Stmt::Goto { .. } => reject_transfer(ctx, stmt.span, "GOTO"),
            Stmt::ComputedGoto { .. } => reject_transfer(ctx, stmt.span, "computed GOTO"),
            Stmt::ArithmeticIf { .. } => reject_transfer(ctx, stmt.span, "arithmetic IF"),
            Stmt::Exit { .. } => reject_transfer(ctx, stmt.span, "EXIT"),
            Stmt::Cycle { .. } => reject_transfer(ctx, stmt.span, "CYCLE"),
            Stmt::Write { controls, .. } | Stmt::Read { controls, .. } => {
                reject_io_branches(ctx, stmt.span, controls)
            }
            Stmt::Open { specs }
            | Stmt::Close { specs }
            | Stmt::Rewind { specs }
            | Stmt::Backspace { specs }
            | Stmt::Endfile { specs }
            | Stmt::Flush { specs }
            | Stmt::Wait { specs } => reject_io_branches(ctx, stmt.span, specs),
            Stmt::Inquire { specs, .. } => reject_io_branches(ctx, stmt.span, specs),
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                validate_structured_block(ctx, then_body);
                for (_, body) in else_ifs {
                    validate_structured_block(ctx, body);
                }
                if let Some(body) = else_body {
                    validate_structured_block(ctx, body);
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::ForallStmt { stmt: action, .. }
            | Stmt::Labeled { stmt: action, .. } => {
                validate_structured_block(ctx, std::slice::from_ref(action.as_ref()));
            }
            Stmt::DoLoop { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::DoConcurrent { body, .. }
            | Stmt::Block { body, .. }
            | Stmt::Associate { body, .. }
            | Stmt::ForallConstruct { body, .. } => validate_structured_block(ctx, body),
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    validate_structured_block(ctx, &case.body);
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        TypeGuard::TypeIs { body, .. }
                        | TypeGuard::ClassIs { body, .. }
                        | TypeGuard::ClassDefault { body } => body,
                    };
                    validate_structured_block(ctx, body);
                }
            }
            Stmt::SelectRank { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        RankGuard::Rank { body, .. }
                        | RankGuard::RankStar { body }
                        | RankGuard::RankDefault { body } => body,
                    };
                    validate_structured_block(ctx, body);
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                validate_structured_block(ctx, body);
                for (_, body) in elsewhere {
                    validate_structured_block(ctx, body);
                }
            }
            // A nested OpenMP region is a distinct structured block. It is
            // validated independently when normal statement validation
            // reaches it.
            Stmt::OpenMp(_) => {}
            _ => {}
        }
    }
}

fn reject_transfer(ctx: &mut Ctx<'_>, span: Span, statement: &str) {
    ctx.error(
        span,
        format!(
            "{} is not yet supported inside an outlined OpenMP PARALLEL region",
            statement
        ),
    );
}

fn reject_io_branches(ctx: &mut Ctx<'_>, span: Span, controls: &[IoControl]) {
    if controls.iter().any(|control| {
        control.keyword.as_deref().is_some_and(|keyword| {
            matches!(keyword.to_ascii_lowercase().as_str(), "err" | "end" | "eor")
        })
    }) {
        ctx.error(
            span,
            "I/O ERR=/END=/EOR= transfer is not yet supported inside an outlined OpenMP PARALLEL region",
        );
    }
}
