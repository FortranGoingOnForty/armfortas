//! Semantic validation for executable OpenMP constructs.
//!
//! The executable slice is intentionally narrow: a `PARALLEL` region may
//! share, privatize, or first-privatize supported numeric/logical storage. It
//! may use `IF`, `NUM_THREADS`,
//! `SHARED`, `PRIVATE`, `FIRSTPRIVATE`, and `DEFAULT(SHARED/NONE)`. Keeping that
//! boundary explicit lets the outliner execute real concurrent regions without
//! pretending that owning descriptors, characters, or derived objects are
//! already implemented.

use std::collections::{HashMap, HashSet};

use crate::ast::openmp::{OpenMpClause, OpenMpConstruct, OpenMpDefault};
use crate::ast::stmt::{IoControl, RankGuard, SpannedStmt, Stmt, TypeGuard};
use crate::lexer::Span;
use crate::sema::symtab::{SymbolKind, SymbolTable, TypeInfo};

use super::core::{
    collect_default_none_nested_block_references, collect_reference_stmts,
    validation_explicit_dim_bounds, validation_expr_rank, validation_expr_type_info, Ctx,
    ProcedureReferenceFacts,
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

    let clause_info = validate_parallel_clauses(ctx, span, clauses);
    let predetermined_private = predetermined_private_names(ctx.st, body);
    validate_data_environment(ctx, body, &clause_info, &predetermined_private);
    validate_structured_block(ctx, body);
}

#[derive(Default)]
struct ParallelClauseInfo {
    explicitly_scoped: HashSet<String>,
    default_none: bool,
}

fn validate_parallel_clauses(
    ctx: &mut Ctx<'_>,
    span: Span,
    clauses: &[OpenMpClause],
) -> ParallelClauseInfo {
    let mut saw_if = false;
    let mut saw_num_threads = false;
    let mut data_attributes = HashMap::new();

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
                for name in names {
                    let key = name.to_ascii_lowercase();
                    register_data_attribute(ctx, span, &mut data_attributes, &key, "SHARED");
                    validate_shared_object(ctx, name, span);
                }
            }
            OpenMpClause::Private(names) => {
                for name in names {
                    let key = name.to_ascii_lowercase();
                    register_data_attribute(ctx, span, &mut data_attributes, &key, "PRIVATE");
                    validate_private_object(ctx, name, span, "PRIVATE");
                }
            }
            OpenMpClause::FirstPrivate(names) => {
                for name in names {
                    let key = name.to_ascii_lowercase();
                    register_data_attribute(ctx, span, &mut data_attributes, &key, "FIRSTPRIVATE");
                    validate_private_object(ctx, name, span, "FIRSTPRIVATE");
                }
            }
            OpenMpClause::Default(OpenMpDefault::Shared) => {}
            OpenMpClause::Default(OpenMpDefault::None) => {}
            unsupported => ctx.error(
                span,
                format!(
                    "OpenMP {} clause on PARALLEL is recognized but not yet implemented",
                    clause_name(unsupported)
                ),
            ),
        }
    }
    ParallelClauseInfo {
        explicitly_scoped: data_attributes.into_keys().collect(),
        default_none: clauses
            .iter()
            .any(|clause| matches!(clause, OpenMpClause::Default(OpenMpDefault::None))),
    }
}

fn register_data_attribute(
    ctx: &mut Ctx<'_>,
    span: Span,
    attributes: &mut HashMap<String, &'static str>,
    name: &str,
    attribute: &'static str,
) {
    if let Some(previous) = attributes.insert(name.to_string(), attribute) {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL variable '{}' appears in both {} and {} data-sharing clauses",
                name, previous, attribute
            ),
        );
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

pub(crate) fn predetermined_private_names(
    st: &SymbolTable,
    body: &[SpannedStmt],
) -> HashSet<String> {
    let mut candidates = Vec::new();
    collect_predetermined_private_names(body, &mut candidates);
    let references = capture_references(st, body);
    candidates
        .into_iter()
        .filter_map(|(name, span)| {
            references
                .iter()
                .any(|(reference, reference_span, role)| {
                    reference == &name && reference_span == &span && *role == ReferenceRole::Value
                })
                .then_some(name)
        })
        .collect()
}

fn collect_predetermined_private_names(stmts: &[SpannedStmt], names: &mut Vec<(String, Span)>) {
    for stmt in stmts {
        match &stmt.node {
            Stmt::DoLoop { var, body, .. } => {
                if let Some(var) = var {
                    names.push((var.to_ascii_lowercase(), stmt.span));
                }
                collect_predetermined_private_names(body, names);
            }
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_predetermined_private_names(then_body, names);
                for (_, body) in else_ifs {
                    collect_predetermined_private_names(body, names);
                }
                if let Some(body) = else_body {
                    collect_predetermined_private_names(body, names);
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::ForallStmt { stmt: action, .. }
            | Stmt::Labeled { stmt: action, .. } => {
                collect_predetermined_private_names(std::slice::from_ref(action.as_ref()), names);
            }
            Stmt::DoWhile { body, .. }
            | Stmt::DoConcurrent { body, .. }
            | Stmt::Block { body, .. }
            | Stmt::Associate { body, .. }
            | Stmt::ForallConstruct { body, .. } => {
                collect_predetermined_private_names(body, names);
            }
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    collect_predetermined_private_names(&case.body, names);
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        TypeGuard::TypeIs { body, .. }
                        | TypeGuard::ClassIs { body, .. }
                        | TypeGuard::ClassDefault { body } => body,
                    };
                    collect_predetermined_private_names(body, names);
                }
            }
            Stmt::SelectRank { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        RankGuard::Rank { body, .. }
                        | RankGuard::RankStar { body }
                        | RankGuard::RankDefault { body } => body,
                    };
                    collect_predetermined_private_names(body, names);
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                collect_predetermined_private_names(body, names);
                for (_, body) in elsewhere {
                    collect_predetermined_private_names(body, names);
                }
            }
            // A nested OpenMP region owns a distinct data environment. Its
            // predetermined variables must not change the enclosing region's
            // classification.
            Stmt::OpenMp(_) => {}
            _ => {}
        }
    }
}

fn validate_data_environment(
    ctx: &mut Ctx<'_>,
    body: &[SpannedStmt],
    clause_info: &ParallelClauseInfo,
    predetermined_private: &HashSet<String>,
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
        if !is_data_reference
            || !seen.insert(name.clone())
            || clause_info.explicitly_scoped.contains(&name)
        {
            continue;
        }

        let Some(symbol) = ctx.lookup_lexical(&name) else {
            validate_shared_object(ctx, &name, span);
            continue;
        };
        let predetermined_shared = symbol.kind == SymbolKind::Parameter
            || symbol
                .attrs
                .array_spec
                .iter()
                .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedSize { .. }));
        if predetermined_shared {
            validate_shared_object(ctx, &name, span);
        } else if predetermined_private.contains(&name) {
            validate_private_object(ctx, &name, span, "predetermined PRIVATE");
        } else if clause_info.default_none {
            ctx.error(
                span,
                format!(
                    "OpenMP DEFAULT(NONE) variable '{}' must appear in a data-sharing clause",
                    name
                ),
            );
        } else {
            validate_shared_object(ctx, &name, span);
        }
    }
}

fn validate_shared_object(ctx: &mut Ctx<'_>, name: &str, span: Span) {
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
    if !matches!(symbol.kind, SymbolKind::Variable | SymbolKind::Parameter) {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL capture of '{}' requires a variable or named constant",
                name
            ),
        );
        return;
    }
    if (symbol.attrs.allocatable || symbol.attrs.pointer) && symbol.attrs.array_spec.is_empty() {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL shared scalar allocatable or pointer '{}' is recognized but not yet implemented",
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
    if !symbol.attrs.array_spec.is_empty() {
        let descriptor_backed_allocatable_or_pointer = (symbol.attrs.allocatable
            || symbol.attrs.pointer)
            && symbol
                .attrs
                .array_spec
                .iter()
                .all(|spec| matches!(spec, crate::ast::decl::ArraySpec::Deferred));
        let constant_explicit_shape = symbol
            .attrs
            .array_spec
            .iter()
            .all(|spec| validation_explicit_dim_bounds(ctx, spec).is_some());
        let is_dummy = is_current_dummy(ctx, symbol, name);
        let runtime_explicit_shape_dummy = is_dummy
            && symbol
                .attrs
                .array_spec
                .iter()
                .all(|spec| matches!(spec, crate::ast::decl::ArraySpec::Explicit { .. }));
        let assumed_shape_dummy = is_dummy
            && symbol.attrs.array_spec.iter().all(|spec| {
                matches!(
                    spec,
                    crate::ast::decl::ArraySpec::AssumedShape { .. }
                        | crate::ast::decl::ArraySpec::Deferred
                )
            });
        let assumed_size_dummy = is_dummy
            && symbol
                .attrs
                .array_spec
                .split_last()
                .is_some_and(|(last, leading)| {
                    matches!(last, crate::ast::decl::ArraySpec::AssumedSize { .. })
                        && leading.iter().all(|spec| {
                            matches!(spec, crate::ast::decl::ArraySpec::Explicit { .. })
                        })
                });
        if !descriptor_backed_allocatable_or_pointer
            && !constant_explicit_shape
            && !runtime_explicit_shape_dummy
            && !assumed_shape_dummy
            && !assumed_size_dummy
        {
            ctx.error(
                span,
                format!(
                    "OpenMP PARALLEL shared array '{}' must currently have constant explicit shape or be a non-optional explicit-shape, assumed-shape, or assumed-size dummy",
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
                    "OpenMP PARALLEL shared array '{}' must currently have INTEGER, REAL, DOUBLE PRECISION, or LOGICAL elements",
                    name
                ),
            );
        }
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

fn is_current_dummy(ctx: &Ctx<'_>, symbol: &crate::sema::symtab::Symbol, name: &str) -> bool {
    let key = name.to_ascii_lowercase();
    ctx.current_args.contains(&key)
        || (symbol.scope == ctx.scope_id
            && ctx
                .st
                .scope(ctx.scope_id)
                .arg_order
                .iter()
                .any(|arg| arg == &key))
}

fn validate_private_object(ctx: &mut Ctx<'_>, name: &str, span: Span, clause: &str) {
    let Some(symbol) = ctx.lookup_lexical(name) else {
        ctx.error(
            span,
            format!(
                "OpenMP {} variable '{}' does not resolve to a visible data object",
                clause, name
            ),
        );
        return;
    };
    if symbol.kind != SymbolKind::Variable {
        ctx.error(
            span,
            format!("OpenMP {} list item '{}' must be a variable", clause, name),
        );
        return;
    }
    let is_dummy = is_current_dummy(ctx, symbol, name);
    if symbol.attrs.optional {
        ctx.error(
            span,
            format!(
                "OpenMP {} OPTIONAL dummy '{}' is recognized but not yet implemented",
                clause, name
            ),
        );
        return;
    }
    if symbol.attrs.pointer || symbol.attrs.target {
        ctx.error(
            span,
            format!(
                "OpenMP {} pointer or target variable '{}' is recognized but not yet implemented",
                clause, name
            ),
        );
        return;
    }
    if symbol.attrs.volatile || symbol.attrs.asynchronous {
        ctx.error(
            span,
            format!(
                "OpenMP {} VOLATILE or ASYNCHRONOUS variable '{}' requires memory-model support that is not yet implemented",
                clause, name
            ),
        );
        return;
    }
    if symbol.attrs.allocatable {
        let deferred_shape_array = !symbol.attrs.array_spec.is_empty()
            && symbol
                .attrs
                .array_spec
                .iter()
                .all(|spec| matches!(spec, crate::ast::decl::ArraySpec::Deferred));
        if !deferred_shape_array {
            ctx.error(
                span,
                format!(
                    "OpenMP {} scalar allocatable '{}' is recognized but not yet implemented",
                    clause, name
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
                    "OpenMP {} allocatable array '{}' must currently have INTEGER, REAL, DOUBLE PRECISION, or LOGICAL elements",
                    clause, name
                ),
            );
        }
        return;
    }
    if is_dummy {
        ctx.error(
            span,
            format!(
                "OpenMP {} dummy argument '{}' is recognized but not yet implemented",
                clause, name
            ),
        );
        return;
    }
    if !symbol.attrs.array_spec.is_empty() {
        if symbol
            .attrs
            .array_spec
            .iter()
            .any(|spec| validation_explicit_dim_bounds(ctx, spec).is_none())
        {
            ctx.error(
                span,
                format!(
                    "OpenMP {} array '{}' must currently have constant explicit shape",
                    clause, name
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
                    "OpenMP {} array '{}' must currently have INTEGER, REAL, DOUBLE PRECISION, or LOGICAL elements",
                    clause, name
                ),
            );
        }
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
                "OpenMP {} variable '{}' must currently be a scalar INTEGER, REAL, DOUBLE PRECISION, or LOGICAL",
                clause, name
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
