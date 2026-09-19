//! Semantic validation for executable OpenMP constructs.
//!
//! The executable slice is intentionally narrow: a `PARALLEL` region may
//! share scalar numeric or logical storage and may use `IF`, `NUM_THREADS`,
//! `SHARED`, and `DEFAULT(SHARED)`. Keeping that boundary explicit lets the
//! outliner execute real concurrent regions without pretending that arrays,
//! characters, derived objects, or private data are already implemented.

use std::collections::HashSet;

use crate::ast::openmp::{OpenMpClause, OpenMpConstruct};
use crate::ast::stmt::{IoControl, RankGuard, SpannedStmt, Stmt, TypeGuard};
use crate::lexer::Span;
use crate::sema::symtab::{SymbolKind, SymbolTable, TypeInfo};

use super::core::{
    collect_default_none_nested_block_references, collect_reference_stmts, validation_expr_rank,
    validation_expr_type_info, Ctx, ProcedureReferenceFacts,
};

pub(crate) use super::core::ReferenceRole;

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

    let explicitly_shared = validate_parallel_clauses(ctx, span, clauses);
    validate_shared_captures(ctx, body, &explicitly_shared);
    validate_structured_block(ctx, body);
}

fn validate_parallel_clauses(
    ctx: &mut Ctx<'_>,
    span: Span,
    clauses: &[OpenMpClause],
) -> HashSet<String> {
    let mut saw_if = false;
    let mut saw_num_threads = false;
    let mut explicitly_shared = HashSet::new();

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
            OpenMpClause::Shared(names) => {
                let mut seen = HashSet::new();
                for name in names {
                    let key = name.to_ascii_lowercase();
                    if !seen.insert(key.clone()) {
                        ctx.error(
                            span,
                            format!("OpenMP SHARED list repeats variable '{}'", name),
                        );
                    }
                    explicitly_shared.insert(key);
                    validate_shared_scalar(ctx, name, span, false);
                }
            }
            OpenMpClause::Default(crate::ast::openmp::OpenMpDefault::Shared) => {}
            unsupported => ctx.error(
                span,
                format!(
                    "OpenMP {} clause on PARALLEL is recognized but not yet implemented",
                    clause_name(unsupported)
                ),
            ),
        }
    }
    explicitly_shared
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

/// Return all references in deterministic source order. Callers classify an
/// ambiguous `name(args)` as callable or data using their resolved scope; the
/// parser cannot distinguish a function call from an array reference alone.
pub(crate) fn capture_references(
    st: &SymbolTable,
    body: &[SpannedStmt],
) -> Vec<(String, Span, ReferenceRole)> {
    let shadowed = HashSet::new();
    let mut facts = ProcedureReferenceFacts::default();
    collect_reference_stmts(body, &shadowed, &mut facts);
    collect_default_none_nested_block_references(st, body, &shadowed, &mut facts);

    facts
        .references
        .into_iter()
        .map(|reference| (reference.name, reference.span, reference.role))
        .collect()
}

fn validate_shared_captures(
    ctx: &mut Ctx<'_>,
    body: &[SpannedStmt],
    explicitly_shared: &HashSet<String>,
) {
    let mut seen = HashSet::new();
    for (name, span, role) in capture_references(ctx.st, body) {
        let is_data_reference = role == ReferenceRole::Value
            || (role == ReferenceRole::Callable
                && ctx.lookup_lexical(&name).is_some_and(|symbol| {
                    matches!(
                        symbol.kind,
                        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::ProcedurePointer
                    )
                }));
        if is_data_reference && seen.insert(name.clone()) && !explicitly_shared.contains(&name) {
            validate_shared_scalar(ctx, &name, span, true);
        }
    }
}

fn validate_shared_scalar(ctx: &mut Ctx<'_>, name: &str, span: Span, allow_named_constant: bool) {
    let Some(symbol) = ctx.lookup_lexical(name) else {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL cannot capture '{}'; ASSOCIATE and unresolved names are not yet supported",
                name
            ),
        );
        return;
    };
    if symbol.kind != SymbolKind::Variable
        && !(allow_named_constant && symbol.kind == SymbolKind::Parameter)
    {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL capture of '{}' requires variable storage; named constants and other entities are not yet supported",
                name
            ),
        );
        return;
    }
    if !symbol.attrs.array_spec.is_empty() {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL shared array '{}' is recognized but not yet implemented",
                name
            ),
        );
        return;
    }
    if symbol.attrs.allocatable || symbol.attrs.pointer {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL shared allocatable or pointer '{}' is recognized but not yet implemented",
                name
            ),
        );
        return;
    }
    if symbol.attrs.optional {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL shared OPTIONAL dummy '{}' is recognized but not yet implemented",
                name
            ),
        );
        return;
    }
    if symbol.attrs.volatile || symbol.attrs.asynchronous {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL shared VOLATILE or ASYNCHRONOUS variable '{}' requires memory-model support that is not yet implemented",
                name
            ),
        );
        return;
    }
    if !matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Integer { .. })
            | Some(TypeInfo::Real { .. })
            | Some(TypeInfo::DoublePrecision)
            | Some(TypeInfo::Logical { .. })
    ) {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL shared variable '{}' must currently be a scalar INTEGER, REAL, DOUBLE PRECISION, or LOGICAL",
                name
            ),
        );
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
