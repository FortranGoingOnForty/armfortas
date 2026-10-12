//! Semantic validation for executable OpenMP constructs.
//!
//! The executable slice is intentionally narrow: `PARALLEL` data environments
//! support selected intrinsic/derived storage, while canonical worksharing
//! `DO` and combined `PARALLEL DO` support the implemented schedules and
//! scalar LASTPRIVATE copy-out and predefined scalar/fixed-rank-one reductions.
//! Named and unnamed `CRITICAL` regions retain process-wide lock identity,
//! `MASTER` regions execute only on the primary thread without an implicit
//! barrier, explicit `BARRIER` directives synchronize the current team, and
//! `ATOMIC READ` and `ATOMIC WRITE` cover scalar intrinsic assignments, while
//! `ATOMIC UPDATE` covers scalar numeric addition.
//! Keeping that boundary explicit lets the outliner execute real concurrent
//! regions without pretending later schedules, loop clauses, or wider object
//! ownership cases are already implemented.

use std::collections::{HashMap, HashSet};

use crate::ast::expr::{Expr, SectionSubscript};
use crate::ast::openmp::{
    OpenMpClause, OpenMpConstruct, OpenMpDefault, OpenMpDoacrossDependence, OpenMpReductionItem,
    OpenMpReductionOperator, OpenMpScheduleKind,
};
use crate::ast::stmt::{IoControl, RankGuard, SpannedStmt, Stmt, TypeGuard};
use crate::lexer::Span;
use crate::sema::symtab::{Intent, SymbolKind, SymbolTable, TypeInfo};

use super::core::{
    collect_default_none_nested_block_references, collect_reference_expr, collect_reference_stmts,
    validation_const_int_value, validation_explicit_dim_bounds, validation_expr_rank,
    validation_expr_type_info, Ctx, ProcedureReferenceFacts,
};

pub(crate) use super::core::ReferenceRole;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorksharingLoopKind {
    Do,
    Loop,
    ParallelDo,
}

impl WorksharingLoopKind {
    fn is_combined(self) -> bool {
        self == Self::ParallelDo
    }

    fn directive_name(self) -> &'static str {
        match self {
            Self::Do => "DO",
            Self::Loop => "LOOP",
            Self::ParallelDo => "PARALLEL DO",
        }
    }
}

pub(super) fn validate_construct(ctx: &mut Ctx<'_>, span: Span, construct: &OpenMpConstruct) {
    match construct {
        OpenMpConstruct::Parallel { clauses, body } => {
            reject_in_pure(ctx, span, "PARALLEL");
            let clause_info = validate_parallel_clauses(ctx, span, clauses, false);
            validate_partial_reduction_references(ctx, clauses, body);
            let standalone_lastprivate =
                standalone_worksharing_lastprivate_names(body, |clauses| {
                    worksharing_collapse_depth_value(ctx, clauses)
                });
            let mut predetermined_private = predetermined_private_names(ctx.st, body, |clauses| {
                worksharing_collapse_depth_value(ctx, clauses)
            });
            predetermined_private.retain(|name| !standalone_lastprivate.contains(name));
            validate_data_environment(ctx, body, &clause_info, &predetermined_private);
            validate_structured_block(ctx, body);
        }
        OpenMpConstruct::Do { clauses, loop_stmt } => {
            reject_in_pure(ctx, span, "DO");
            validate_worksharing_loop(ctx, span, clauses, loop_stmt, WorksharingLoopKind::Do);
            validate_partial_reduction_references(
                ctx,
                clauses,
                std::slice::from_ref(loop_stmt.as_ref()),
            );
        }
        OpenMpConstruct::Loop { clauses, loop_stmt } => {
            reject_in_pure(ctx, span, "LOOP");
            if ctx.openmp_parallel_depth == 0 {
                ctx.error(
                    span,
                    "OpenMP LOOP without BIND must be nested inside an OpenMP PARALLEL region",
                );
            }
            for clause in clauses {
                if !matches!(
                    clause,
                    OpenMpClause::Private(_)
                        | OpenMpClause::LastPrivate { .. }
                        | OpenMpClause::Collapse(_)
                        | OpenMpClause::Reduction { .. }
                ) {
                    ctx.error(
                        span,
                        format!(
                            "OpenMP {} clause on LOOP is recognized but not yet implemented",
                            clause_name(clause)
                        ),
                    );
                }
            }
            validate_worksharing_loop(ctx, span, clauses, loop_stmt, WorksharingLoopKind::Loop);
            validate_partial_reduction_references(
                ctx,
                clauses,
                std::slice::from_ref(loop_stmt.as_ref()),
            );
        }
        OpenMpConstruct::ParallelDo { clauses, loop_stmt } => {
            reject_in_pure(ctx, span, "PARALLEL DO");
            let clause_info = validate_parallel_clauses(ctx, span, clauses, true);
            let loop_body = std::slice::from_ref(loop_stmt.as_ref());
            let predetermined_private = predetermined_private_names(ctx.st, loop_body, |clauses| {
                worksharing_collapse_depth_value(ctx, clauses)
            });
            validate_data_environment(ctx, loop_body, &clause_info, &predetermined_private);
            validate_worksharing_loop(
                ctx,
                span,
                clauses,
                loop_stmt,
                WorksharingLoopKind::ParallelDo,
            );
            validate_partial_reduction_references(ctx, clauses, loop_body);
        }
        OpenMpConstruct::Critical { body, .. } => {
            reject_in_pure(ctx, span, "CRITICAL");
            validate_structured_block(ctx, body);
        }
        OpenMpConstruct::Master { body } => {
            reject_in_pure(ctx, span, "MASTER");
            if let Some(enclosing) = ctx.openmp_construct_stack.last().copied() {
                if matches!(enclosing, "DO" | "LOOP" | "PARALLEL DO" | "ATOMIC UPDATE") {
                    ctx.error(
                        span,
                        format!(
                            "OpenMP MASTER may not be closely nested inside an OpenMP {enclosing} region"
                        ),
                    );
                }
            }
            validate_structured_block(ctx, body);
        }
        OpenMpConstruct::Single { clauses, body } => {
            reject_in_pure(ctx, span, "SINGLE");
            if let Some(enclosing) = ctx.openmp_construct_stack.last().copied() {
                if matches!(
                    enclosing,
                    "DO" | "LOOP"
                        | "PARALLEL DO"
                        | "CRITICAL"
                        | "MASTER"
                        | "SINGLE"
                        | "ORDERED"
                        | "ATOMIC UPDATE"
                ) {
                    ctx.error(
                        span,
                        format!(
                            "OpenMP SINGLE may not be closely nested inside an OpenMP {enclosing} region"
                        ),
                    );
                }
            }
            for clause in clauses {
                if !matches!(clause, OpenMpClause::Nowait) {
                    ctx.error(
                        span,
                        format!(
                            "OpenMP {} clause on SINGLE is recognized but not yet implemented",
                            clause_name(clause)
                        ),
                    );
                }
            }
            validate_structured_block(ctx, body);
        }
        OpenMpConstruct::Ordered { body } => {
            reject_in_pure(ctx, span, "ORDERED");
            if !matches!(
                ctx.openmp_construct_stack.last().copied(),
                Some("DO" | "PARALLEL DO")
            ) {
                ctx.error(
                    span,
                    "OpenMP ORDERED must be closely nested inside an OpenMP DO or PARALLEL DO region",
                );
            } else if ctx.openmp_ordered_binding_depth == 0 {
                ctx.error(
                    span,
                    "the binding OpenMP worksharing loop must specify the ORDERED clause",
                );
            }
            validate_structured_block(ctx, body);
        }
        OpenMpConstruct::OrderedDoacross { dependence } => {
            reject_in_pure(ctx, span, "ORDERED DOACROSS");
            if !matches!(
                ctx.openmp_construct_stack.last().copied(),
                Some("DO" | "PARALLEL DO")
            ) {
                ctx.error(
                    span,
                    "OpenMP ORDERED DOACROSS must be closely nested inside an OpenMP DO or PARALLEL DO region",
                );
            } else if ctx.openmp_doacross_binding_depth == 0 {
                ctx.error(
                    span,
                    "the binding OpenMP worksharing loop must specify a parameterized ORDERED clause",
                );
            }
            for coordinate in dependence.vectors().iter().flatten() {
                if validation_expr_rank(ctx, coordinate) != Some(0)
                    || !matches!(
                        validation_expr_type_info(ctx, coordinate),
                        Some(TypeInfo::Integer { .. })
                    )
                {
                    ctx.error(
                        coordinate.span,
                        "OpenMP ORDERED sink coordinates must be scalar INTEGER expressions",
                    );
                }
            }
        }
        OpenMpConstruct::Barrier => {
            reject_in_pure(ctx, span, "BARRIER");
            if let Some(enclosing) = ctx.openmp_construct_stack.last().copied() {
                if matches!(
                    enclosing,
                    "DO" | "LOOP" | "PARALLEL DO" | "CRITICAL" | "MASTER" | "ATOMIC UPDATE"
                ) {
                    ctx.error(
                        span,
                        format!(
                            "OpenMP BARRIER may not be closely nested inside an OpenMP {enclosing} region"
                        ),
                    );
                }
            }
        }
        OpenMpConstruct::Flush { variables } => {
            reject_in_pure(ctx, span, "FLUSH");
            let mut seen = HashSet::new();
            for variable in variables {
                let key = variable.to_ascii_lowercase();
                if !seen.insert(key) {
                    ctx.error(
                        span,
                        format!("OpenMP FLUSH list item '{variable}' appears more than once"),
                    );
                    continue;
                }
                let Some(symbol) = ctx.lookup_lexical(variable) else {
                    ctx.error(
                        span,
                        format!("OpenMP FLUSH list item '{variable}' is not declared"),
                    );
                    continue;
                };
                if symbol.kind != SymbolKind::Variable {
                    ctx.error(
                        span,
                        format!("OpenMP FLUSH list item '{variable}' must name a variable"),
                    );
                }
            }
        }
        OpenMpConstruct::AtomicRead { assignment } => {
            reject_in_pure(ctx, span, "ATOMIC READ");
            validate_atomic_read(ctx, assignment);
        }
        OpenMpConstruct::AtomicWrite { assignment } => {
            reject_in_pure(ctx, span, "ATOMIC WRITE");
            validate_atomic_write(ctx, assignment);
        }
        OpenMpConstruct::AtomicUpdate { assignment } => {
            reject_in_pure(ctx, span, "ATOMIC UPDATE");
            validate_atomic_update(ctx, span, assignment);
        }
    }
}

fn validate_atomic_read(ctx: &mut Ctx<'_>, assignment: &SpannedStmt) {
    let Stmt::Assignment { target, value } = &assignment.node else {
        ctx.error(
            assignment.span,
            "OpenMP ATOMIC READ must be followed by an assignment",
        );
        return;
    };
    let Some(destination) =
        validate_atomic_storage_designator(ctx, "READ", "destination", target, true)
    else {
        return;
    };
    let Some(source) = validate_atomic_storage_designator(ctx, "READ", "source", value, true)
    else {
        return;
    };
    if target.to_sexpr().eq_ignore_ascii_case(&value.to_sexpr()) {
        ctx.error(
            assignment.span,
            "OpenMP ATOMIC READ source and destination must designate distinct storage locations",
        );
        return;
    }
    if !destination.eq_ignore_ascii_case(&source)
        && (atomic_expr_references_name(value, &destination)
            || atomic_expr_references_name(target, &source))
    {
        ctx.error(
            assignment.span,
            "OpenMP ATOMIC READ source and destination designators may not reference each other",
        );
    }
}

fn validate_atomic_write(ctx: &mut Ctx<'_>, assignment: &SpannedStmt) {
    let Stmt::Assignment { target, value } = &assignment.node else {
        ctx.error(
            assignment.span,
            "OpenMP ATOMIC WRITE must be followed by an assignment",
        );
        return;
    };
    let Some(target_name) =
        validate_atomic_storage_designator(ctx, "WRITE", "target", target, true)
    else {
        return;
    };
    if validation_expr_rank(ctx, value) != Some(0)
        || !atomic_scalar_type_supported(ctx, value, true)
    {
        ctx.error(
            value.span,
            "OpenMP ATOMIC WRITE expression must be a scalar non-CHARACTER intrinsic value",
        );
        return;
    }
    if atomic_expr_references_name(value, &target_name) {
        ctx.error(
            value.span,
            format!("OpenMP ATOMIC WRITE expression may not reference its target '{target_name}'"),
        );
    }
}

fn validate_atomic_storage_designator(
    ctx: &mut Ctx<'_>,
    operation: &str,
    role: &str,
    expr: &crate::ast::expr::SpannedExpr,
    allow_logical: bool,
) -> Option<String> {
    let Some(base_name) = atomic_target_base_name(expr) else {
        ctx.error(
            expr.span,
            format!(
                "OpenMP ATOMIC {operation} {role} must be a scalar variable or side-effect-free array element"
            ),
        );
        return None;
    };
    let Some(symbol) = ctx.lookup_lexical(base_name) else {
        ctx.error(
            expr.span,
            format!("OpenMP ATOMIC {operation} {role} '{base_name}' is not declared"),
        );
        return None;
    };
    if symbol.kind != SymbolKind::Variable || validation_expr_rank(ctx, expr) != Some(0) {
        ctx.error(
            expr.span,
            format!("OpenMP ATOMIC {operation} {role} must designate one scalar storage location"),
        );
        return None;
    }
    if !atomic_scalar_type_supported(ctx, expr, allow_logical) {
        ctx.error(
            expr.span,
            format!(
                "OpenMP ATOMIC {operation} {role} must have a supported non-CHARACTER intrinsic type"
            ),
        );
        return None;
    }
    Some(base_name.to_ascii_lowercase())
}

fn atomic_scalar_type_supported(
    ctx: &Ctx<'_>,
    expr: &crate::ast::expr::SpannedExpr,
    allow_logical: bool,
) -> bool {
    match validation_expr_type_info(ctx, expr) {
        Some(TypeInfo::Integer { kind }) => matches!(kind.unwrap_or(4), 1 | 2 | 4 | 8),
        Some(TypeInfo::Real { kind }) => matches!(
            kind.unwrap_or_else(crate::driver::defaults::default_real_kind),
            4 | 8
        ),
        Some(TypeInfo::DoublePrecision) => true,
        Some(TypeInfo::Complex { kind }) => matches!(
            kind.unwrap_or_else(crate::driver::defaults::default_real_kind),
            4 | 8
        ),
        Some(TypeInfo::Logical { kind }) if allow_logical => {
            matches!(kind.unwrap_or(4), 1 | 2 | 4 | 8)
        }
        _ => false,
    }
}

fn atomic_expr_references_name(expr: &crate::ast::expr::SpannedExpr, name: &str) -> bool {
    let mut facts = ProcedureReferenceFacts::default();
    collect_reference_expr(expr, &HashSet::new(), &mut facts);
    facts
        .references
        .iter()
        .any(|reference| reference.name.eq_ignore_ascii_case(name))
}

fn validate_atomic_update(ctx: &mut Ctx<'_>, _span: Span, assignment: &SpannedStmt) {
    let Stmt::Assignment { target, value } = &assignment.node else {
        ctx.error(
            assignment.span,
            "OpenMP ATOMIC UPDATE must be followed by an assignment",
        );
        return;
    };
    let Some(base_name) = atomic_target_base_name(target) else {
        ctx.error(
            target.span,
            "OpenMP ATOMIC UPDATE target must be a scalar variable or side-effect-free array element",
        );
        return;
    };
    let Some(symbol) = ctx.lookup_lexical(base_name) else {
        ctx.error(
            target.span,
            format!("OpenMP ATOMIC UPDATE target '{base_name}' is not declared"),
        );
        return;
    };
    if symbol.kind != SymbolKind::Variable || validation_expr_rank(ctx, target) != Some(0) {
        ctx.error(
            target.span,
            "OpenMP ATOMIC UPDATE target must designate one scalar storage location",
        );
        return;
    }
    let supported_type = match validation_expr_type_info(ctx, target) {
        Some(TypeInfo::Integer { kind }) => matches!(kind.unwrap_or(4), 1 | 2 | 4 | 8),
        Some(TypeInfo::Real { kind }) => matches!(
            kind.unwrap_or_else(crate::driver::defaults::default_real_kind),
            4 | 8
        ),
        Some(TypeInfo::DoublePrecision) => true,
        Some(TypeInfo::Complex { kind }) => matches!(
            kind.unwrap_or_else(crate::driver::defaults::default_real_kind),
            4 | 8
        ),
        _ => false,
    };
    if !supported_type {
        ctx.error(
            target.span,
            "OpenMP ATOMIC UPDATE addition currently requires a scalar INTEGER, REAL, DOUBLE PRECISION, or COMPLEX storage location",
        );
        return;
    }
    let Expr::BinaryOp {
        op: crate::ast::expr::BinaryOp::Add,
        left,
        right,
    } = &value.node
    else {
        ctx.error(
            value.span,
            "OpenMP ATOMIC UPDATE currently supports only 'x = x + expr' addition",
        );
        return;
    };
    let target_sexpr = target.to_sexpr();
    let mut update_left = left.as_ref();
    let mut update_terms = vec![right.as_ref()];
    while !target_sexpr.eq_ignore_ascii_case(&update_left.to_sexpr()) {
        let Expr::BinaryOp {
            op: crate::ast::expr::BinaryOp::Add,
            left,
            right,
        } = &update_left.node
        else {
            ctx.error(
                update_left.span,
                "OpenMP ATOMIC UPDATE left operand must match the assignment target",
            );
            return;
        };
        update_terms.push(right.as_ref());
        update_left = left.as_ref();
    }
    for update_term in update_terms {
        let mut facts = ProcedureReferenceFacts::default();
        collect_reference_expr(update_term, &HashSet::new(), &mut facts);
        if facts
            .references
            .iter()
            .any(|reference| reference.name.eq_ignore_ascii_case(base_name))
        {
            ctx.error(
                update_term.span,
                format!(
                    "OpenMP ATOMIC UPDATE expression may not otherwise reference '{base_name}'"
                ),
            );
            return;
        }
    }
}

fn atomic_target_base_name(target: &crate::ast::expr::SpannedExpr) -> Option<&str> {
    match &target.node {
        Expr::Name { name } => Some(name),
        Expr::FunctionCall { callee, args }
            if !args.is_empty()
                && args.iter().all(|argument| {
                    argument.keyword.is_none()
                        && matches!(
                            &argument.value,
                            SectionSubscript::Element(expr)
                                if atomic_subscript_is_side_effect_free(expr)
                        )
                }) =>
        {
            match &callee.node {
                Expr::Name { name } => Some(name),
                _ => None,
            }
        }
        _ => None,
    }
}

fn atomic_subscript_is_side_effect_free(expr: &crate::ast::expr::SpannedExpr) -> bool {
    match &expr.node {
        Expr::IntegerLiteral { .. } | Expr::Name { .. } => true,
        Expr::UnaryOp { operand, .. } => atomic_subscript_is_side_effect_free(operand),
        Expr::BinaryOp { left, right, .. } => {
            atomic_subscript_is_side_effect_free(left)
                && atomic_subscript_is_side_effect_free(right)
        }
        Expr::ParenExpr { inner } => atomic_subscript_is_side_effect_free(inner),
        _ => false,
    }
}

fn validate_partial_reduction_references(
    ctx: &mut Ctx<'_>,
    clauses: &[OpenMpClause],
    body: &[SpannedStmt],
) {
    let mut facts = ProcedureReferenceFacts::default();
    collect_reference_stmts(body, &HashSet::new(), &mut facts);
    for item in clauses.iter().flat_map(|clause| {
        clause
            .reduction_items()
            .into_iter()
            .flatten()
            .filter(|item| matches!(item, OpenMpReductionItem::ArrayDesignator { .. }))
    }) {
        let Some(symbol) = ctx.lookup_lexical(item.base_name()) else {
            continue;
        };
        let Some((selected_lower, selected_upper, declared_lower, declared_upper)) =
            validated_reduction_selection_bounds(ctx, item, symbol)
        else {
            continue;
        };
        if selected_lower == declared_lower && selected_upper == declared_upper {
            continue;
        }
        let key = item.base_name().to_ascii_lowercase();
        for reference in facts
            .references
            .iter()
            .filter(|reference| reference.name == key)
        {
            let Some(subscripts) = reference.subscripts.as_deref() else {
                ctx.error(
                    reference.span,
                    format!(
                        "OpenMP partial REDUCTION item '{}' may not currently be referenced as the whole array inside the region",
                        item.source_text()
                    ),
                );
                continue;
            };
            if subscripts.len() != 1 {
                continue;
            }
            match &subscripts[0].value {
                SectionSubscript::Element(index) => {
                    if let Some(index) = validation_const_int_value(ctx, index) {
                        if index < selected_lower || index > selected_upper {
                            ctx.error(
                                reference.span,
                                format!(
                                    "OpenMP partial REDUCTION item '{}' does not include element {}",
                                    item.source_text(),
                                    index
                                ),
                            );
                        }
                    }
                }
                SectionSubscript::Range { start, end, stride } => {
                    let stride = match stride {
                        Some(stride) => validation_const_int_value(ctx, stride),
                        None => Some(1),
                    };
                    let start = match start {
                        Some(start) => validation_const_int_value(ctx, start),
                        None => Some(declared_lower),
                    };
                    let end = match end {
                        Some(end) => validation_const_int_value(ctx, end),
                        None => Some(declared_upper),
                    };
                    let (Some(stride), Some(start), Some(end)) = (stride, start, end) else {
                        ctx.error(
                            reference.span,
                            format!(
                                "OpenMP partial REDUCTION item '{}' requires constant bounds on section references inside the region",
                                item.source_text()
                            ),
                        );
                        continue;
                    };
                    if stride != 1 || start < selected_lower || end > selected_upper {
                        ctx.error(
                            reference.span,
                            format!(
                                "OpenMP partial REDUCTION item '{}' does not include referenced section {}",
                                item.source_text(),
                                subscripts[0].value.to_sexpr()
                            ),
                        );
                    }
                }
            }
        }
    }
}

fn validated_reduction_selection_bounds(
    ctx: &Ctx<'_>,
    item: &OpenMpReductionItem,
    symbol: &crate::sema::symtab::Symbol,
) -> Option<(i128, i128, i128, i128)> {
    let (declared_lower, declared_upper) =
        validation_explicit_dim_bounds(ctx, symbol.attrs.array_spec.first()?)?;
    let OpenMpReductionItem::ArrayDesignator { subscripts, .. } = item else {
        return Some((
            declared_lower,
            declared_upper,
            declared_lower,
            declared_upper,
        ));
    };
    let subscript = &subscripts.first()?.value;
    let (selected_lower, selected_upper) = match subscript {
        SectionSubscript::Element(index) => {
            let index = validation_const_int_value(ctx, index)?;
            (index, index)
        }
        SectionSubscript::Range { start, end, .. } => (
            start
                .as_ref()
                .map(|start| validation_const_int_value(ctx, start))
                .unwrap_or(Some(declared_lower))?,
            end.as_ref()
                .map(|end| validation_const_int_value(ctx, end))
                .unwrap_or(Some(declared_upper))?,
        ),
    };
    Some((
        selected_lower,
        selected_upper,
        declared_lower,
        declared_upper,
    ))
}

fn reject_in_pure(ctx: &mut Ctx<'_>, span: Span, construct: &str) {
    if ctx.in_pure {
        ctx.error(
            span,
            format!("OpenMP {construct} is not allowed in a PURE procedure"),
        );
    }
}

struct ParallelClauseInfo {
    explicitly_scoped: HashSet<String>,
    default_data_sharing: Option<OpenMpDefault>,
}

pub(super) fn parallel_private_names(
    ctx: &Ctx<'_>,
    clauses: &[OpenMpClause],
    body: &[SpannedStmt],
) -> HashSet<String> {
    let mut private = clauses
        .iter()
        .flat_map(|clause| match clause {
            OpenMpClause::Private(names) | OpenMpClause::FirstPrivate(names) => {
                names.iter().map(String::as_str).collect::<Vec<_>>()
            }
            OpenMpClause::Reduction { items, .. } => {
                items.iter().map(OpenMpReductionItem::base_name).collect()
            }
            _ => Vec::new(),
        })
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    if !clauses
        .iter()
        .any(|clause| matches!(clause, OpenMpClause::Default(OpenMpDefault::Private)))
    {
        return private;
    }

    let explicitly_scoped = clauses
        .iter()
        .flat_map(|clause| match clause {
            OpenMpClause::Shared(names)
            | OpenMpClause::Private(names)
            | OpenMpClause::FirstPrivate(names)
            | OpenMpClause::CopyIn(names) => names.iter().map(String::as_str).collect::<Vec<_>>(),
            OpenMpClause::LastPrivate { variables, .. } => {
                variables.iter().map(String::as_str).collect()
            }
            OpenMpClause::Reduction { items, .. } => {
                items.iter().map(OpenMpReductionItem::base_name).collect()
            }
            _ => Vec::new(),
        })
        .map(str::to_ascii_lowercase)
        .collect::<HashSet<_>>();
    let standalone_lastprivate = standalone_worksharing_lastprivate_names(body, |clauses| {
        worksharing_collapse_depth_value(ctx, clauses)
    });
    private.extend(
        predetermined_private_names(ctx.st, body, |clauses| {
            worksharing_collapse_depth_value(ctx, clauses)
        })
        .into_iter()
        .filter(|name| !standalone_lastprivate.contains(name)),
    );

    for (name, _, role) in capture_references(ctx.st, body) {
        let is_data_reference = role == ReferenceRole::Value
            || (role == ReferenceRole::Callable
                && ctx
                    .lookup_lexical(&name)
                    .is_some_and(|symbol| callable_reference_needs_capture(ctx.st, symbol, &name)));
        if !is_data_reference || explicitly_scoped.contains(&name) {
            continue;
        }
        let Some(symbol) = ctx.lookup_lexical(&name) else {
            continue;
        };
        if symbol.attrs.openmp_threadprivate {
            continue;
        }
        let predetermined_shared = symbol.kind == SymbolKind::Parameter
            || symbol
                .attrs
                .array_spec
                .iter()
                .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedSize { .. }));
        if !predetermined_shared {
            private.insert(name);
        }
    }
    private
}

fn validate_parallel_clauses(
    ctx: &mut Ctx<'_>,
    span: Span,
    clauses: &[OpenMpClause],
    combined_do: bool,
) -> ParallelClauseInfo {
    let mut saw_if = false;
    let mut saw_num_threads = false;
    let mut saw_default = false;
    let mut default_data_sharing = None;
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
            OpenMpClause::CopyIn(names) => {
                for name in names {
                    let key = name.to_ascii_lowercase();
                    register_data_attribute(ctx, span, &mut data_attributes, &key, "COPYIN");
                    validate_copyin_object(ctx, name, span);
                }
            }
            OpenMpClause::LastPrivate {
                conditional,
                variables,
            } if combined_do => {
                if *conditional {
                    ctx.error(
                        span,
                        "OpenMP LASTPRIVATE(CONDITIONAL:) is recognized but not yet implemented",
                    );
                }
                for name in variables {
                    let key = name.to_ascii_lowercase();
                    register_data_attribute(ctx, span, &mut data_attributes, &key, "LASTPRIVATE");
                    validate_lastprivate_object(ctx, name, span);
                }
            }
            OpenMpClause::Reduction { operator, items } => {
                for item in items {
                    let name = item.base_name();
                    let key = name.to_ascii_lowercase();
                    register_data_attribute(ctx, span, &mut data_attributes, &key, "REDUCTION");
                    validate_reduction_object(ctx, item, span, *operator);
                }
            }
            OpenMpClause::Default(value) => {
                if std::mem::replace(&mut saw_default, true) {
                    ctx.error(span, "OpenMP PARALLEL may not repeat the DEFAULT clause");
                }
                match value {
                    OpenMpDefault::Shared | OpenMpDefault::Private | OpenMpDefault::None => {
                        default_data_sharing = Some(*value)
                    }
                    OpenMpDefault::FirstPrivate => ctx.error(
                        span,
                        "OpenMP DEFAULT(FIRSTPRIVATE) clause on PARALLEL is recognized but not yet implemented",
                    ),
                }
            }
            OpenMpClause::Schedule { .. } if combined_do => {}
            OpenMpClause::Collapse(_) if combined_do => {}
            OpenMpClause::Ordered(_) if combined_do => {}
            OpenMpClause::Nowait if combined_do => {
                ctx.error(span, "OpenMP PARALLEL DO may not specify NOWAIT")
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
    ParallelClauseInfo {
        explicitly_scoped: data_attributes.into_keys().collect(),
        default_data_sharing,
    }
}

struct AssociatedLoop<'a> {
    name: Option<&'a str>,
    var: &'a str,
    start: &'a crate::ast::expr::SpannedExpr,
    end: &'a crate::ast::expr::SpannedExpr,
    step: Option<&'a crate::ast::expr::SpannedExpr>,
    body: &'a [SpannedStmt],
}

fn validate_associated_loop<'a>(
    ctx: &mut Ctx<'_>,
    directive_span: Span,
    loop_stmt: &'a SpannedStmt,
    description: &str,
) -> Option<AssociatedLoop<'a>> {
    let Stmt::DoLoop {
        name,
        var,
        start,
        end,
        step,
        body,
        ..
    } = &loop_stmt.node
    else {
        ctx.error(
            directive_span,
            format!("OpenMP {description} must be a counted DO loop"),
        );
        return None;
    };
    let (Some(var), Some(start), Some(end)) = (var.as_deref(), start.as_ref(), end.as_ref()) else {
        ctx.error(
            directive_span,
            format!("OpenMP {description} must be a counted DO loop"),
        );
        return None;
    };

    let Some(symbol) = ctx.lookup_lexical(var) else {
        ctx.error(
            loop_stmt.span,
            format!("OpenMP {description} iteration variable '{var}' is not declared"),
        );
        return None;
    };
    if symbol.kind != SymbolKind::Variable
        || !symbol.attrs.array_spec.is_empty()
        || !matches!(symbol.type_info.as_ref(), Some(TypeInfo::Integer { .. }))
    {
        ctx.error(
            loop_stmt.span,
            format!(
                "OpenMP {description} iteration variable '{var}' must be a scalar INTEGER variable"
            ),
        );
    }
    for (label, expr) in [("lower bound", start), ("upper bound", end)]
        .into_iter()
        .chain(step.iter().map(|expr| ("increment", expr)))
    {
        if validation_expr_rank(ctx, expr) != Some(0)
            || !matches!(
                validation_expr_type_info(ctx, expr),
                Some(TypeInfo::Integer { .. })
            )
        {
            ctx.error(
                expr.span,
                format!("OpenMP {description} {label} must be a scalar INTEGER expression"),
            );
        }
    }

    Some(AssociatedLoop {
        name: name.as_deref(),
        var,
        start,
        end,
        step: step.as_ref(),
        body,
    })
}

fn validate_collapse_depth(
    ctx: &mut Ctx<'_>,
    span: Span,
    clauses: &[OpenMpClause],
    kind: WorksharingLoopKind,
) -> usize {
    let mut depth = 1;
    let mut saw_collapse = false;
    for clause in clauses {
        let OpenMpClause::Collapse(argument) = clause else {
            continue;
        };
        if std::mem::replace(&mut saw_collapse, true) {
            ctx.error(
                span,
                format!(
                    "OpenMP {} may not repeat the COLLAPSE clause",
                    kind.directive_name()
                ),
            );
            continue;
        }
        if validation_expr_rank(ctx, argument) != Some(0)
            || !matches!(
                validation_expr_type_info(ctx, argument),
                Some(TypeInfo::Integer { .. })
            )
        {
            ctx.error(
                argument.span,
                "OpenMP COLLAPSE argument must be a scalar INTEGER constant expression",
            );
            continue;
        }
        match validation_const_int_value(ctx, argument) {
            Some(value @ 1..=2) => depth = value as usize,
            Some(value) if value <= 0 => {
                ctx.error(argument.span, "OpenMP COLLAPSE argument must be positive")
            }
            Some(value) => ctx.error(
                argument.span,
                format!(
                    "OpenMP COLLAPSE({value}) is recognized but only COLLAPSE(2) is implemented"
                ),
            ),
            None => ctx.error(
                argument.span,
                "OpenMP COLLAPSE argument must be a constant expression",
            ),
        }
    }
    depth
}

fn worksharing_collapse_depth_value(ctx: &Ctx<'_>, clauses: &[OpenMpClause]) -> i64 {
    clauses
        .iter()
        .find_map(|clause| match clause {
            OpenMpClause::Collapse(argument) => validation_const_int_value(ctx, argument)
                .and_then(|value| i64::try_from(value).ok()),
            _ => None,
        })
        .unwrap_or(1)
}

fn expression_references_name(expr: &crate::ast::expr::SpannedExpr, name: &str) -> bool {
    let mut facts = ProcedureReferenceFacts::default();
    collect_reference_expr(expr, &HashSet::new(), &mut facts);
    let key = name.to_ascii_lowercase();
    facts
        .references
        .iter()
        .any(|reference| reference.name == key)
}

fn expression_is_name(expr: &crate::ast::expr::SpannedExpr, name: &str) -> bool {
    matches!(
        &expr.node,
        Expr::Name { name: candidate } if candidate.eq_ignore_ascii_case(name)
    )
}

fn integer_variable_kind(ctx: &Ctx<'_>, name: &str) -> Option<u8> {
    let symbol = ctx.lookup_lexical(name)?;
    let TypeInfo::Integer { kind } = symbol.type_info.as_ref()? else {
        return None;
    };
    Some(kind.unwrap_or(crate::driver::defaults::default_int_kind()))
}

fn validate_worksharing_loop(
    ctx: &mut Ctx<'_>,
    span: Span,
    clauses: &[OpenMpClause],
    loop_stmt: &SpannedStmt,
    kind: WorksharingLoopKind,
) {
    let combined_do = kind.is_combined();
    let directive_name = kind.directive_name();
    let Some(outer_loop) = validate_associated_loop(ctx, span, loop_stmt, directive_name) else {
        return;
    };
    let collapse_depth = validate_collapse_depth(ctx, span, clauses, kind);
    validate_ordered_worksharing_shape(ctx, span, clauses, &outer_loop, kind, collapse_depth);
    validate_doacross_worksharing_shape(ctx, span, clauses, &outer_loop, kind, collapse_depth);
    let mut associated_loop_keys = HashSet::from([outer_loop.var.to_ascii_lowercase()]);
    if collapse_depth == 2 {
        let inner_stmt = if outer_loop.body.len() == 1 {
            outer_loop.body.first()
        } else {
            None
        };
        let Some(inner_stmt) = inner_stmt else {
            ctx.error(
                loop_stmt.span,
                "OpenMP COLLAPSE(2) requires a perfectly nested second counted DO loop",
            );
            return;
        };
        let Some(inner_loop) =
            validate_associated_loop(ctx, span, inner_stmt, "COLLAPSE(2) associated loop")
        else {
            return;
        };
        if inner_loop.var.eq_ignore_ascii_case(outer_loop.var) {
            ctx.error(
                inner_stmt.span,
                "OpenMP COLLAPSE(2) associated loops must use distinct iteration variables",
            );
        }
        let lower_depends_on_outer = expression_references_name(inner_loop.start, outer_loop.var);
        let upper_depends_on_outer = expression_references_name(inner_loop.end, outer_loop.var);
        for bound in [inner_loop.start, inner_loop.end] {
            if expression_references_name(bound, outer_loop.var)
                && !expression_is_name(bound, outer_loop.var)
            {
                ctx.error(
                    bound.span,
                    "OpenMP nonrectangular COLLAPSE(2) bounds currently support only a direct reference to the outer iteration variable",
                );
            }
        }
        if inner_loop
            .step
            .is_some_and(|step| expression_references_name(step, outer_loop.var))
        {
            ctx.error(
                inner_loop.step.expect("checked OpenMP loop increment").span,
                "OpenMP COLLAPSE(2) inner increment may not reference the outer iteration variable",
            );
        }
        let nonrectangular = lower_depends_on_outer || upper_depends_on_outer;
        if kind == WorksharingLoopKind::Loop && nonrectangular {
            ctx.error(
                span,
                "OpenMP LOOP nonrectangular COLLAPSE(2) is recognized but not yet implemented",
            );
        }
        if nonrectangular
            && integer_variable_kind(ctx, outer_loop.var)
                != integer_variable_kind(ctx, inner_loop.var)
        {
            ctx.error(
                inner_stmt.span,
                "OpenMP nonrectangular COLLAPSE(2) iteration variables must have the same INTEGER kind",
            );
        }
        if nonrectangular
            && clauses
                .iter()
                .any(|clause| matches!(clause, OpenMpClause::LastPrivate { .. }))
        {
            ctx.error(
                span,
                "OpenMP LASTPRIVATE on a nonrectangular COLLAPSE(2) loop is recognized but not yet implemented",
            );
        }
        associated_loop_keys.insert(inner_loop.var.to_ascii_lowercase());
    }

    let lastprivate_names: HashSet<_> = clauses
        .iter()
        .filter_map(|clause| match clause {
            OpenMpClause::LastPrivate { variables, .. } => Some(variables.as_slice()),
            _ => None,
        })
        .flatten()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    if !combined_do {
        let mut data_attributes = HashMap::new();
        for clause in clauses {
            let (names, attribute) = match clause {
                OpenMpClause::Private(names) => (Some(names.as_slice()), "PRIVATE"),
                OpenMpClause::FirstPrivate(names) => (Some(names.as_slice()), "FIRSTPRIVATE"),
                OpenMpClause::LastPrivate { variables, .. } => {
                    (Some(variables.as_slice()), "LASTPRIVATE")
                }
                OpenMpClause::Reduction { items, .. } => {
                    for item in items {
                        register_data_attribute(
                            ctx,
                            span,
                            &mut data_attributes,
                            &item.base_name().to_ascii_lowercase(),
                            "REDUCTION",
                        );
                    }
                    (None, "")
                }
                _ => (None, ""),
            };
            for name in names.into_iter().flatten() {
                register_data_attribute(
                    ctx,
                    span,
                    &mut data_attributes,
                    &name.to_ascii_lowercase(),
                    attribute,
                );
            }
        }
    }

    for clause in clauses {
        match clause {
            OpenMpClause::Schedule {
                kind,
                chunk_size,
            }
                if *kind == OpenMpScheduleKind::Static
                    || (*kind == OpenMpScheduleKind::Dynamic && combined_do) =>
            {
                if let Some(chunk_size) = chunk_size {
                    if validation_expr_rank(ctx, chunk_size) != Some(0)
                        || !matches!(
                            validation_expr_type_info(ctx, chunk_size),
                            Some(TypeInfo::Integer { .. })
                        )
                    {
                        ctx.error(
                            chunk_size.span,
                            "OpenMP SCHEDULE chunk size must be a scalar INTEGER expression",
                        );
                    }
                    if validation_const_int_value(ctx, chunk_size).is_some_and(|value| value <= 0)
                    {
                        ctx.error(
                            chunk_size.span,
                            "OpenMP SCHEDULE chunk size must be positive",
                        );
                    }
                }
            }
            OpenMpClause::Nowait => {}
            OpenMpClause::Schedule { kind, .. } => ctx.error(
                span,
                format!(
                    "OpenMP SCHEDULE({}) is recognized but not yet implemented",
                    schedule_kind_name(*kind)
                ),
            ),
            OpenMpClause::Private(names)
                if !combined_do
                    && names
                        .iter()
                        .all(|name| associated_loop_keys.contains(&name.to_ascii_lowercase())) => {}
            OpenMpClause::Private(names) if !combined_do => {
                for name in names {
                    if !associated_loop_keys.contains(&name.to_ascii_lowercase()) {
                        validate_worksharing_private_object(ctx, name, span);
                    }
                }
            }
            OpenMpClause::FirstPrivate(names)
                if !combined_do
                    && names
                        .iter()
                        .any(|name| associated_loop_keys.contains(&name.to_ascii_lowercase())) => ctx.error(
                            span,
                            format!(
                                "OpenMP {directive_name} associated iteration variables may not appear in FIRSTPRIVATE"
                            ),
                        ),
            OpenMpClause::FirstPrivate(names)
                if !combined_do
                    && names
                        .iter()
                        .all(|name| lastprivate_names.contains(&name.to_ascii_lowercase())) => {}
            OpenMpClause::FirstPrivate(_) if !combined_do => ctx.error(
                    span,
                    format!(
                        "OpenMP {} clause on DO is recognized but not yet implemented",
                        clause_name(clause)
                    ),
                ),
            OpenMpClause::FirstPrivate(names)
                if combined_do
                    && names
                        .iter()
                        .any(|name| associated_loop_keys.contains(&name.to_ascii_lowercase())) => ctx.error(
                            span,
                            "OpenMP PARALLEL DO associated iteration variables may not appear in FIRSTPRIVATE",
                        ),
            OpenMpClause::Shared(names)
                if combined_do
                    && names
                        .iter()
                        .any(|name| associated_loop_keys.contains(&name.to_ascii_lowercase())) => ctx.error(
                            span,
                            "OpenMP PARALLEL DO associated iteration variables may not appear in SHARED",
                        ),
            OpenMpClause::Private(_)
            | OpenMpClause::FirstPrivate(_)
            | OpenMpClause::CopyIn(_)
            | OpenMpClause::LastPrivate { .. }
            | OpenMpClause::Shared(_)
            | OpenMpClause::Default(_)
            | OpenMpClause::If { .. }
            | OpenMpClause::NumThreads(_)
                if combined_do => {}
            OpenMpClause::LastPrivate {
                conditional,
                variables,
            } => {
                if *conditional {
                    ctx.error(
                        span,
                        "OpenMP LASTPRIVATE(CONDITIONAL:) is recognized but not yet implemented",
                    );
                }
                for name in variables {
                    let key = name.to_ascii_lowercase();
                    if kind == WorksharingLoopKind::Loop
                        && !associated_loop_keys.contains(&key)
                    {
                        ctx.error(
                            span,
                            format!(
                                "OpenMP LOOP LASTPRIVATE variable '{name}' must be an associated iteration variable"
                            ),
                        );
                        continue;
                    }
                    if ctx
                        .openmp_parallel_private_frames
                        .last()
                        .is_some_and(|frame| frame.contains(&key))
                    {
                        ctx.error(
                            span,
                            format!(
                                "OpenMP {directive_name} LASTPRIVATE variable '{name}' may not be private in the binding PARALLEL region"
                            ),
                        );
                    }
                    validate_lastprivate_object(ctx, name, span);
                }
            }
            OpenMpClause::Collapse(_) => {}
            OpenMpClause::Ordered(_) => {}
            OpenMpClause::Reduction { items, .. } if items
                .iter()
                .any(|item| associated_loop_keys.contains(&item.base_name().to_ascii_lowercase())) =>
            {
                ctx.error(
                    span,
                    format!(
                        "OpenMP {directive_name} associated iteration variables may not appear in REDUCTION"
                    ),
                )
            }
            OpenMpClause::Reduction { operator, items }
                if kind == WorksharingLoopKind::Loop
                    && !loop_reduction_frontier_supported(ctx, *operator, items) =>
            {
                ctx.error(
                    span,
                    "OpenMP LOOP REDUCTION currently supports only the + operator with scalar INTEGER list items",
                )
            }
            OpenMpClause::Reduction { .. } if combined_do => {}
            OpenMpClause::Reduction {
                operator,
                items,
            } => {
                for item in items {
                    let name = item.base_name();
                    let key = name.to_ascii_lowercase();
                    if ctx
                        .openmp_parallel_private_frames
                        .last()
                        .is_some_and(|frame| frame.contains(&key))
                    {
                        ctx.error(
                            span,
                            format!(
                                "OpenMP {directive_name} REDUCTION variable '{name}' must be shared in the binding PARALLEL region"
                            ),
                        );
                    }
                    validate_reduction_object(ctx, item, span, *operator);
                }
            }
            OpenMpClause::Shared(_)
            | OpenMpClause::Default(_)
            | OpenMpClause::If { .. }
            | OpenMpClause::NumThreads(_) => unreachable!(
                "parser admitted a parallel-only clause on standalone OpenMP {directive_name}"
            ),
            OpenMpClause::Private(_)
            | OpenMpClause::FirstPrivate(_)
            | OpenMpClause::CopyIn(_) => {
                unreachable!(
                    "OpenMP {directive_name} data clause escaped construct-specific validation"
                )
            }
        }
    }
    let mut cycle_targets = Vec::new();
    if collapse_depth == 1 {
        cycle_targets.push(outer_loop.name.map(str::to_ascii_lowercase));
    }
    let mut internal_labels = HashSet::new();
    collect_structured_block_labels(outer_loop.body, &mut internal_labels);
    validate_structured_block_with_cycle_targets(
        ctx,
        outer_loop.body,
        &mut cycle_targets,
        &internal_labels,
    );
}

fn validate_ordered_worksharing_shape(
    ctx: &mut Ctx<'_>,
    span: Span,
    clauses: &[OpenMpClause],
    outer_loop: &AssociatedLoop<'_>,
    kind: WorksharingLoopKind,
    collapse_depth: usize,
) {
    if !clauses
        .iter()
        .any(|clause| matches!(clause, OpenMpClause::Ordered(None)))
    {
        return;
    }
    if collapse_depth != 1 {
        ctx.error(
            span,
            "OpenMP ORDERED with COLLAPSE is recognized but not yet implemented",
        );
    }
    if clauses.iter().any(|clause| {
        matches!(
            clause,
            OpenMpClause::Schedule {
                kind,
                ..
            } if *kind != OpenMpScheduleKind::Static
        )
    }) {
        ctx.error(
            span,
            "OpenMP ORDERED currently supports only the default or STATIC schedule",
        );
    }
    if !matches!(
        outer_loop.body,
        [SpannedStmt {
            node: Stmt::OpenMp(OpenMpConstruct::Ordered { .. }),
            ..
        }]
    ) {
        ctx.error(
            span,
            format!(
                "OpenMP {} ORDERED currently requires exactly one directly nested unconditional ORDERED region per logical iteration",
                kind.directive_name()
            ),
        );
    }
}

fn validate_doacross_worksharing_shape(
    ctx: &mut Ctx<'_>,
    span: Span,
    clauses: &[OpenMpClause],
    outer_loop: &AssociatedLoop<'_>,
    kind: WorksharingLoopKind,
    collapse_depth: usize,
) {
    let parameters = clauses
        .iter()
        .filter_map(|clause| match clause {
            OpenMpClause::Ordered(Some(parameter)) => Some(parameter),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some(parameter) = parameters.first().copied() else {
        return;
    };
    if parameters.len() > 1 {
        ctx.error(
            span,
            format!(
                "OpenMP {} may not repeat the ORDERED clause",
                kind.directive_name()
            ),
        );
    }
    if validation_expr_rank(ctx, parameter) != Some(0)
        || !matches!(
            validation_expr_type_info(ctx, parameter),
            Some(TypeInfo::Integer { .. })
        )
    {
        ctx.error(
            parameter.span,
            "OpenMP ORDERED parameter must be a scalar INTEGER constant expression",
        );
        return;
    }
    match validation_const_int_value(ctx, parameter) {
        Some(2) => {}
        Some(value) if value <= 0 => {
            ctx.error(parameter.span, "OpenMP ORDERED parameter must be positive");
            return;
        }
        Some(value) => {
            ctx.error(
                parameter.span,
                format!(
                    "OpenMP ORDERED({value}) is recognized but only two-dimensional doacross loops are implemented"
                ),
            );
            return;
        }
        None => {
            ctx.error(
                parameter.span,
                "OpenMP ORDERED parameter must be a constant expression",
            );
            return;
        }
    }
    if collapse_depth != 1 {
        ctx.error(
            span,
            "OpenMP ORDERED(2) with COLLAPSE is recognized but not yet implemented",
        );
    }
    if clauses.iter().any(|clause| {
        matches!(
            clause,
            OpenMpClause::Schedule {
                kind,
                ..
            } if *kind != OpenMpScheduleKind::Static
        )
    }) {
        ctx.error(
            span,
            "OpenMP ORDERED(2) currently supports only the default or STATIC schedule",
        );
    }
    let [inner_stmt] = outer_loop.body else {
        ctx.error(
            span,
            "OpenMP ORDERED(2) requires a perfectly nested second counted DO loop",
        );
        return;
    };
    let Some(inner_loop) =
        validate_associated_loop(ctx, span, inner_stmt, "ORDERED(2) associated loop")
    else {
        return;
    };
    if inner_loop.var.eq_ignore_ascii_case(outer_loop.var) {
        ctx.error(
            inner_stmt.span,
            "OpenMP ORDERED(2) associated loops must use distinct iteration variables",
        );
    }
    if expression_references_name(inner_loop.start, outer_loop.var)
        || expression_references_name(inner_loop.end, outer_loop.var)
        || inner_loop
            .step
            .is_some_and(|step| expression_references_name(step, outer_loop.var))
    {
        ctx.error(
            inner_stmt.span,
            "OpenMP ORDERED(2) currently requires a rectangular associated loop nest",
        );
    }
    let Some(outer_step) = doacross_constant_step(ctx, outer_loop) else {
        ctx.error(
            outer_loop.step.map_or(span, |step| step.span),
            "OpenMP ORDERED(2) outer increment must currently be a nonzero constant expression",
        );
        return;
    };
    let Some(inner_step) = doacross_constant_step(ctx, &inner_loop) else {
        ctx.error(
            inner_loop.step.map_or(inner_stmt.span, |step| step.span),
            "OpenMP ORDERED(2) inner increment must currently be a nonzero constant expression",
        );
        return;
    };

    let mut sink_directives = 0usize;
    let mut source_directives = 0usize;
    let mut saw_source = false;
    for statement in inner_loop.body {
        match &statement.node {
            Stmt::OpenMp(OpenMpConstruct::OrderedDoacross {
                dependence: OpenMpDoacrossDependence::Sink { vectors },
            }) => {
                sink_directives += 1;
                if saw_source {
                    ctx.error(
                        statement.span,
                        "OpenMP ORDERED sink dependencies must precede the source dependency in the implemented doacross form",
                    );
                }
                for vector in vectors {
                    validate_doacross_sink_vector(
                        ctx,
                        statement.span,
                        vector,
                        [outer_loop.var, inner_loop.var],
                        [outer_step, inner_step],
                    );
                }
            }
            Stmt::OpenMp(OpenMpConstruct::OrderedDoacross {
                dependence: OpenMpDoacrossDependence::Source,
            }) => {
                source_directives += 1;
                saw_source = true;
            }
            Stmt::OpenMp(OpenMpConstruct::Ordered { .. }) => ctx.error(
                statement.span,
                "a parameterized OpenMP ORDERED loop must use stand-alone source/sink dependencies",
            ),
            _ => {}
        }
    }
    if sink_directives == 0 || source_directives != 1 {
        ctx.error(
            span,
            "OpenMP ORDERED(2) currently requires directly nested sink dependencies followed by exactly one source dependency in every inner iteration",
        );
    }
}

fn doacross_constant_step(ctx: &Ctx<'_>, associated: &AssociatedLoop<'_>) -> Option<i128> {
    let step = associated
        .step
        .map_or(Some(1), |step| validation_const_int_value(ctx, step))?;
    (step != 0).then_some(step)
}

fn doacross_coordinate_offset(
    ctx: &Ctx<'_>,
    coordinate: &crate::ast::expr::SpannedExpr,
    variable: &str,
) -> Option<i128> {
    match &coordinate.node {
        Expr::Name { name } if name.eq_ignore_ascii_case(variable) => Some(0),
        Expr::ParenExpr { inner } => doacross_coordinate_offset(ctx, inner, variable),
        Expr::BinaryOp { op, left, right }
            if expression_is_name(left, variable)
                && matches!(
                    op,
                    crate::ast::expr::BinaryOp::Add | crate::ast::expr::BinaryOp::Sub
                ) =>
        {
            let offset = validation_const_int_value(ctx, right)?;
            Some(if matches!(op, crate::ast::expr::BinaryOp::Sub) {
                -offset
            } else {
                offset
            })
        }
        _ => None,
    }
}

fn validate_doacross_sink_vector(
    ctx: &mut Ctx<'_>,
    span: Span,
    vector: &[crate::ast::expr::SpannedExpr],
    variables: [&str; 2],
    steps: [i128; 2],
) {
    if vector.len() != 2 {
        ctx.error(
            span,
            format!(
                "OpenMP ORDERED(2) sink vectors must contain exactly two coordinates, found {}",
                vector.len()
            ),
        );
        return;
    }
    let mut first_logical_offset = None;
    for ((coordinate, variable), step) in vector.iter().zip(variables).zip(steps) {
        let Some(offset) = doacross_coordinate_offset(ctx, coordinate, variable) else {
            ctx.error(
                coordinate.span,
                format!(
                    "OpenMP ORDERED sink coordinate must have the form {variable}, {variable}+constant, or {variable}-constant"
                ),
            );
            continue;
        };
        if offset % step != 0 {
            ctx.error(
                coordinate.span,
                format!(
                    "OpenMP ORDERED sink offset for '{variable}' must align with its loop increment"
                ),
            );
            continue;
        }
        let logical_offset = offset / step;
        if first_logical_offset.is_none() && logical_offset != 0 {
            first_logical_offset = Some(logical_offset);
        }
    }
    if first_logical_offset.is_none_or(|offset| offset >= 0) {
        ctx.error(
            span,
            "OpenMP ORDERED sink vector must identify a lexicographically earlier logical iteration",
        );
    }
}

fn validate_lastprivate_object(ctx: &mut Ctx<'_>, name: &str, span: Span) {
    validate_private_object(ctx, name, span, "LASTPRIVATE");
    let Some(symbol) = ctx.lookup_lexical(name) else {
        return;
    };
    if symbol.kind != SymbolKind::Variable {
        return;
    }
    if !symbol.attrs.array_spec.is_empty() || symbol.attrs.allocatable || symbol.attrs.pointer {
        ctx.error(
            span,
            format!(
                "OpenMP LASTPRIVATE variable '{name}' must currently be a nonallocatable, nonpointer scalar"
            ),
        );
        return;
    }
    let supported = matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Integer { .. })
            | Some(TypeInfo::Real { .. })
            | Some(TypeInfo::DoublePrecision)
            | Some(TypeInfo::Logical { .. })
    ) || matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Character {
            len: Some(_),
            kind,
        }) if kind.unwrap_or(1) == 1
    );
    if !supported {
        ctx.error(
            span,
            format!(
                "OpenMP LASTPRIVATE variable '{name}' must currently be a scalar INTEGER, REAL, DOUBLE PRECISION, LOGICAL, or fixed-length default-kind CHARACTER"
            ),
        );
    }
}

fn validate_worksharing_private_object(ctx: &mut Ctx<'_>, name: &str, span: Span) {
    validate_private_object(ctx, name, span, "PRIVATE");
    let Some(symbol) = ctx.lookup_lexical(name) else {
        return;
    };
    let supported_type = matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Integer { .. })
            | Some(TypeInfo::Real { .. })
            | Some(TypeInfo::DoublePrecision)
            | Some(TypeInfo::Complex { .. })
            | Some(TypeInfo::Logical { .. })
    );
    if symbol.kind == SymbolKind::Variable
        && (!symbol.attrs.array_spec.is_empty()
            || symbol.attrs.allocatable
            || symbol.attrs.pointer
            || !supported_type)
    {
        ctx.error(
            span,
            format!(
                "OpenMP worksharing PRIVATE variable '{name}' must currently be a nonallocatable, nonpointer scalar INTEGER, REAL, DOUBLE PRECISION, COMPLEX, or LOGICAL"
            ),
        );
    }
}

fn loop_reduction_frontier_supported(
    ctx: &Ctx<'_>,
    operator: OpenMpReductionOperator,
    items: &[OpenMpReductionItem],
) -> bool {
    operator == OpenMpReductionOperator::Add
        && items.iter().all(|item| {
            let OpenMpReductionItem::Name(name) = item else {
                return false;
            };
            let Some(symbol) = ctx.lookup_lexical(name) else {
                // Let the general reduction validator issue the unresolved-name
                // diagnostic instead of obscuring it with a frontier error.
                return true;
            };
            if symbol.kind != SymbolKind::Variable {
                // Likewise, preserve the more precise non-variable diagnostic.
                return true;
            }
            symbol.attrs.array_spec.is_empty()
                && matches!(symbol.type_info.as_ref(), Some(TypeInfo::Integer { .. }))
        })
}

fn validate_reduction_object(
    ctx: &mut Ctx<'_>,
    item: &OpenMpReductionItem,
    span: Span,
    operator: OpenMpReductionOperator,
) {
    let name = item.base_name();
    let Some(symbol) = ctx.lookup_lexical(name) else {
        ctx.error(
            span,
            format!("OpenMP REDUCTION variable '{name}' does not resolve to a visible data object"),
        );
        return;
    };
    if symbol.kind != SymbolKind::Variable {
        ctx.error(
            span,
            format!("OpenMP REDUCTION list item '{name}' must be a variable"),
        );
        return;
    }
    if symbol.attrs.openmp_threadprivate {
        ctx.error(
            span,
            format!("OpenMP THREADPRIVATE variable '{name}' may not appear in a REDUCTION clause"),
        );
        return;
    }
    if symbol.attrs.allocatable || symbol.attrs.pointer {
        ctx.error(
            span,
            format!(
                "OpenMP REDUCTION variable '{name}' must currently be nonallocatable and nonpointer"
            ),
        );
        return;
    }
    if symbol.attrs.optional
        || symbol.attrs.volatile
        || symbol.attrs.asynchronous
        || symbol.attrs.intent == Some(Intent::In)
    {
        ctx.error(
            span,
            format!(
                "OpenMP REDUCTION variable '{name}' may not currently be OPTIONAL, VOLATILE, ASYNCHRONOUS, or INTENT(IN)"
            ),
        );
        return;
    }

    let is_array = !symbol.attrs.array_spec.is_empty();
    match (is_array, item) {
        (false, OpenMpReductionItem::Name(_)) => {}
        (false, OpenMpReductionItem::ArrayDesignator { .. }) => {
            ctx.error(
                span,
                format!(
                    "OpenMP REDUCTION list item '{}' must designate an array",
                    item.source_text()
                ),
            );
            return;
        }
        (true, _) => {
            if !validate_rank_one_reduction_selection(ctx, item, symbol, span) {
                return;
            }
        }
    }

    let valid = match (operator, symbol.type_info.as_ref()) {
        (
            OpenMpReductionOperator::Add
            | OpenMpReductionOperator::Multiply
            | OpenMpReductionOperator::Max
            | OpenMpReductionOperator::Min
            | OpenMpReductionOperator::Iand
            | OpenMpReductionOperator::Ior
            | OpenMpReductionOperator::Ieor,
            Some(TypeInfo::Integer { kind }),
        ) => matches!(kind.unwrap_or(4), 1 | 2 | 4 | 8),
        (
            OpenMpReductionOperator::Add
            | OpenMpReductionOperator::Multiply
            | OpenMpReductionOperator::Max
            | OpenMpReductionOperator::Min,
            Some(TypeInfo::Real { kind }),
        ) => matches!(
            kind.unwrap_or_else(crate::driver::defaults::default_real_kind),
            4 | 8
        ),
        (
            OpenMpReductionOperator::Add
            | OpenMpReductionOperator::Multiply
            | OpenMpReductionOperator::Max
            | OpenMpReductionOperator::Min,
            Some(TypeInfo::DoublePrecision),
        ) => true,
        (
            OpenMpReductionOperator::Add | OpenMpReductionOperator::Multiply,
            Some(TypeInfo::Complex { kind }),
        ) => matches!(
            kind.unwrap_or_else(crate::driver::defaults::default_real_kind),
            4 | 8
        ),
        (
            OpenMpReductionOperator::And
            | OpenMpReductionOperator::Or
            | OpenMpReductionOperator::Eqv
            | OpenMpReductionOperator::Neqv,
            Some(TypeInfo::Logical { kind }),
        ) => matches!(kind.unwrap_or(4), 1 | 2 | 4 | 8),
        _ => false,
    };
    if !valid {
        let requirement = reduction_type_requirement(operator, is_array);
        ctx.error(
            span,
            format!(
                "OpenMP {} REDUCTION currently requires {requirement}",
                reduction_operator_name(operator),
            ),
        );
    }
}

fn validate_rank_one_reduction_selection(
    ctx: &mut Ctx<'_>,
    item: &OpenMpReductionItem,
    symbol: &crate::sema::symtab::Symbol,
    span: Span,
) -> bool {
    let name = item.base_name();
    if symbol.attrs.array_spec.len() != 1 {
        ctx.error(
            span,
            format!("OpenMP REDUCTION array '{name}' must currently have rank one"),
        );
        return false;
    }
    let Some((declared_lower, declared_upper)) =
        validation_explicit_dim_bounds(ctx, &symbol.attrs.array_spec[0])
    else {
        ctx.error(
            span,
            format!("OpenMP REDUCTION array '{name}' must currently have constant explicit shape"),
        );
        return false;
    };
    if declared_upper < declared_lower {
        ctx.error(
            span,
            format!("OpenMP REDUCTION array '{name}' may not have zero size"),
        );
        return false;
    }

    let OpenMpReductionItem::ArrayDesignator { subscripts, .. } = item else {
        return true;
    };
    if subscripts.len() != 1 {
        ctx.error(
            span,
            format!(
                "OpenMP REDUCTION list item '{}' must have exactly one subscript",
                item.source_text()
            ),
        );
        return false;
    }
    match &subscripts[0].value {
        SectionSubscript::Element(index) => {
            let Some(index) = validation_const_int_value(ctx, index) else {
                ctx.error(
                    span,
                    format!(
                        "OpenMP REDUCTION array element '{}' must use a constant subscript",
                        item.source_text()
                    ),
                );
                return false;
            };
            validate_reduction_bounds(
                ctx,
                item,
                index,
                index,
                declared_lower,
                declared_upper,
                span,
            )
        }
        SectionSubscript::Range { start, end, stride } => {
            let stride = match stride {
                Some(stride) => validation_const_int_value(ctx, stride),
                None => Some(1),
            };
            let Some(stride) = stride else {
                ctx.error(
                    span,
                    format!(
                        "OpenMP REDUCTION array section '{}' must use a constant unit stride",
                        item.source_text()
                    ),
                );
                return false;
            };
            if stride != 1 {
                ctx.error(
                    span,
                    format!(
                        "OpenMP REDUCTION array section '{}' must currently use unit stride",
                        item.source_text()
                    ),
                );
                return false;
            }
            let start = match start {
                Some(start) => validation_const_int_value(ctx, start),
                None => Some(declared_lower),
            };
            let end = match end {
                Some(end) => validation_const_int_value(ctx, end),
                None => Some(declared_upper),
            };
            let (Some(start), Some(end)) = (start, end) else {
                ctx.error(
                    span,
                    format!(
                        "OpenMP REDUCTION array section '{}' must use constant bounds",
                        item.source_text()
                    ),
                );
                return false;
            };
            if end < start {
                ctx.error(
                    span,
                    format!(
                        "OpenMP REDUCTION array section '{}' may not have zero length",
                        item.source_text()
                    ),
                );
                return false;
            }
            validate_reduction_bounds(ctx, item, start, end, declared_lower, declared_upper, span)
        }
    }
}

fn validate_reduction_bounds(
    ctx: &mut Ctx<'_>,
    item: &OpenMpReductionItem,
    selected_lower: i128,
    selected_upper: i128,
    declared_lower: i128,
    declared_upper: i128,
    span: Span,
) -> bool {
    if selected_lower < declared_lower || selected_upper > declared_upper {
        ctx.error(
            span,
            format!(
                "OpenMP REDUCTION list item '{}' is outside the declared bounds {}:{}",
                item.source_text(),
                declared_lower,
                declared_upper
            ),
        );
        false
    } else {
        true
    }
}

fn reduction_type_requirement(operator: OpenMpReductionOperator, is_array: bool) -> &'static str {
    match (operator, is_array) {
        (
            OpenMpReductionOperator::Iand
            | OpenMpReductionOperator::Ior
            | OpenMpReductionOperator::Ieor,
            false,
        ) => "a scalar INTEGER of kind 1, 2, 4, or 8",
        (
            OpenMpReductionOperator::Iand
            | OpenMpReductionOperator::Ior
            | OpenMpReductionOperator::Ieor,
            true,
        ) => "rank-one INTEGER elements of kind 1, 2, 4, or 8",
        (
            OpenMpReductionOperator::And
            | OpenMpReductionOperator::Or
            | OpenMpReductionOperator::Eqv
            | OpenMpReductionOperator::Neqv,
            false,
        ) => "a scalar LOGICAL of kind 1, 2, 4, or 8",
        (
            OpenMpReductionOperator::Add | OpenMpReductionOperator::Multiply,
            false,
        ) => {
            "a scalar INTEGER of kind 1, 2, 4, or 8, REAL of kind 4 or 8, or COMPLEX of kind 4 or 8"
        }
        (OpenMpReductionOperator::Max | OpenMpReductionOperator::Min, false) => {
            "a scalar INTEGER of kind 1, 2, 4, or 8, or REAL of kind 4 or 8"
        }
        (
            OpenMpReductionOperator::And
            | OpenMpReductionOperator::Or
            | OpenMpReductionOperator::Eqv
            | OpenMpReductionOperator::Neqv,
            true,
        ) => "rank-one LOGICAL elements of kind 1, 2, 4, or 8",
        (OpenMpReductionOperator::Add | OpenMpReductionOperator::Multiply, true) => {
            "rank-one INTEGER elements of kind 1, 2, 4, or 8, REAL elements of kind 4 or 8, or COMPLEX elements of kind 4 or 8"
        }
        (OpenMpReductionOperator::Max | OpenMpReductionOperator::Min, true) => {
            "rank-one INTEGER elements of kind 1, 2, 4, or 8, or REAL elements of kind 4 or 8"
        }
    }
}

fn reduction_operator_name(operator: OpenMpReductionOperator) -> &'static str {
    match operator {
        OpenMpReductionOperator::Add => "+",
        OpenMpReductionOperator::Multiply => "*",
        OpenMpReductionOperator::Max => "MAX",
        OpenMpReductionOperator::Min => "MIN",
        OpenMpReductionOperator::And => ".AND.",
        OpenMpReductionOperator::Or => ".OR.",
        OpenMpReductionOperator::Eqv => ".EQV.",
        OpenMpReductionOperator::Neqv => ".NEQV.",
        OpenMpReductionOperator::Iand => "IAND",
        OpenMpReductionOperator::Ior => "IOR",
        OpenMpReductionOperator::Ieor => "IEOR",
    }
}

fn schedule_kind_name(kind: OpenMpScheduleKind) -> &'static str {
    match kind {
        OpenMpScheduleKind::Static => "STATIC",
        OpenMpScheduleKind::Dynamic => "DYNAMIC",
        OpenMpScheduleKind::Guided => "GUIDED",
        OpenMpScheduleKind::Runtime => "RUNTIME",
        OpenMpScheduleKind::Auto => "AUTO",
    }
}

fn register_data_attribute(
    ctx: &mut Ctx<'_>,
    span: Span,
    attributes: &mut HashMap<String, Vec<&'static str>>,
    name: &str,
    attribute: &'static str,
) {
    let existing = attributes.entry(name.to_string()).or_default();
    let compatible_first_last = (attribute == "FIRSTPRIVATE"
        && existing.as_slice() == ["LASTPRIVATE"])
        || (attribute == "LASTPRIVATE" && existing.as_slice() == ["FIRSTPRIVATE"]);
    if attribute == "REDUCTION" && existing.contains(&"REDUCTION") {
        ctx.error(
            span,
            format!(
                "OpenMP REDUCTION base '{}' may currently appear only once on a directive",
                name
            ),
        );
        existing.push(attribute);
        return;
    }
    if !existing.is_empty() && !compatible_first_last {
        let previous = existing.join(" and ");
        ctx.error(
            span,
            format!(
                "OpenMP variable '{}' appears in both {} and {} data-sharing clauses",
                name, previous, attribute
            ),
        );
    }
    existing.push(attribute);
}

fn clause_name(clause: &OpenMpClause) -> &'static str {
    match clause {
        OpenMpClause::Private(_) => "PRIVATE",
        OpenMpClause::FirstPrivate(_) => "FIRSTPRIVATE",
        OpenMpClause::CopyIn(_) => "COPYIN",
        OpenMpClause::LastPrivate { .. } => "LASTPRIVATE",
        OpenMpClause::Shared(_) => "SHARED",
        OpenMpClause::Default(_) => "DEFAULT",
        OpenMpClause::If { .. } => "IF",
        OpenMpClause::NumThreads(_) => "NUM_THREADS",
        OpenMpClause::Schedule { .. } => "SCHEDULE",
        OpenMpClause::Collapse(_) => "COLLAPSE",
        OpenMpClause::Ordered(_) => "ORDERED",
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

/// Decide whether an ambiguous callable reference carries runtime storage
/// into an outlined region.  Array references, procedure pointers, and dummy
/// procedures do; a named external procedure does not.  Typed EXTERNAL
/// functions are represented as `Variable + attrs.external`, so checking the
/// symbol kind alone would incorrectly capture names such as NPB's `randlc`.
pub(crate) fn callable_reference_needs_capture(
    st: &SymbolTable,
    symbol: &crate::sema::symtab::Symbol,
    name: &str,
) -> bool {
    match symbol.kind {
        SymbolKind::ProcedurePointer => true,
        SymbolKind::Variable | SymbolKind::Parameter => {
            if !symbol.attrs.external {
                return true;
            }
            let key = name.to_ascii_lowercase();
            st.scope(symbol.scope)
                .arg_order
                .iter()
                .any(|argument| argument == &key)
        }
        _ => false,
    }
}

pub(crate) fn predetermined_private_names<F>(
    st: &SymbolTable,
    body: &[SpannedStmt],
    mut collapse_depth: F,
) -> HashSet<String>
where
    F: FnMut(&[OpenMpClause]) -> i64,
{
    let mut candidates = Vec::new();
    collect_predetermined_private_names(body, &mut candidates, &mut collapse_depth);
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

pub(crate) fn standalone_worksharing_lastprivate_names<F>(
    body: &[SpannedStmt],
    mut collapse_depth: F,
) -> HashSet<String>
where
    F: FnMut(&[OpenMpClause]) -> i64,
{
    let mut names = HashSet::new();
    collect_standalone_worksharing_lastprivate_names(body, &mut names, &mut collapse_depth);
    names
}

fn collect_standalone_worksharing_lastprivate_names<F>(
    stmts: &[SpannedStmt],
    names: &mut HashSet<String>,
    collapse_depth: &mut F,
) where
    F: FnMut(&[OpenMpClause]) -> i64,
{
    for stmt in stmts {
        match &stmt.node {
            Stmt::OpenMp(OpenMpConstruct::Do { clauses, .. }) => {
                for clause in clauses {
                    if let OpenMpClause::LastPrivate { variables, .. } = clause {
                        names.extend(variables.iter().map(|name| name.to_ascii_lowercase()));
                    }
                }
            }
            Stmt::OpenMp(OpenMpConstruct::Loop { clauses, loop_stmt }) => {
                let explicitly_private: HashSet<_> = clauses
                    .iter()
                    .filter_map(|clause| match clause {
                        OpenMpClause::Private(variables) => Some(variables.as_slice()),
                        _ => None,
                    })
                    .flatten()
                    .map(|name| name.to_ascii_lowercase())
                    .collect();
                for clause in clauses {
                    if let OpenMpClause::LastPrivate { variables, .. } = clause {
                        names.extend(variables.iter().map(|name| name.to_ascii_lowercase()));
                    }
                }
                let Stmt::DoLoop {
                    var: Some(outer),
                    body,
                    ..
                } = &loop_stmt.node
                else {
                    unreachable!("non-canonical OpenMP LOOP passed semantic validation")
                };
                let outer = outer.to_ascii_lowercase();
                if !explicitly_private.contains(&outer) {
                    names.insert(outer);
                }
                if collapse_depth(clauses) == 2 {
                    let [inner_stmt] = body.as_slice() else {
                        unreachable!("non-perfect OpenMP LOOP COLLAPSE(2) passed validation")
                    };
                    let Stmt::DoLoop {
                        var: Some(inner), ..
                    } = &inner_stmt.node
                    else {
                        unreachable!("non-canonical OpenMP LOOP COLLAPSE(2) passed validation")
                    };
                    let inner = inner.to_ascii_lowercase();
                    if !explicitly_private.contains(&inner) {
                        names.insert(inner);
                    }
                }
            }
            // Nested parallel and combined constructs own distinct data
            // environments; their LASTPRIVATE clauses do not affect this
            // enclosing callback's captures.
            Stmt::OpenMp(_) => {}
            Stmt::DoLoop { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::DoConcurrent { body, .. }
            | Stmt::Block { body, .. }
            | Stmt::Associate { body, .. }
            | Stmt::ForallConstruct { body, .. } => {
                collect_standalone_worksharing_lastprivate_names(body, names, collapse_depth)
            }
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_standalone_worksharing_lastprivate_names(then_body, names, collapse_depth);
                for (_, body) in else_ifs {
                    collect_standalone_worksharing_lastprivate_names(body, names, collapse_depth);
                }
                if let Some(body) = else_body {
                    collect_standalone_worksharing_lastprivate_names(body, names, collapse_depth);
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::ForallStmt { stmt: action, .. }
            | Stmt::Labeled { stmt: action, .. } => {
                collect_standalone_worksharing_lastprivate_names(
                    std::slice::from_ref(action.as_ref()),
                    names,
                    collapse_depth,
                )
            }
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    collect_standalone_worksharing_lastprivate_names(
                        &case.body,
                        names,
                        collapse_depth,
                    );
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        TypeGuard::TypeIs { body, .. }
                        | TypeGuard::ClassIs { body, .. }
                        | TypeGuard::ClassDefault { body } => body,
                    };
                    collect_standalone_worksharing_lastprivate_names(body, names, collapse_depth);
                }
            }
            Stmt::SelectRank { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        RankGuard::Rank { body, .. }
                        | RankGuard::RankStar { body }
                        | RankGuard::RankDefault { body } => body,
                    };
                    collect_standalone_worksharing_lastprivate_names(body, names, collapse_depth);
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                collect_standalone_worksharing_lastprivate_names(body, names, collapse_depth);
                for (_, body) in elsewhere {
                    collect_standalone_worksharing_lastprivate_names(body, names, collapse_depth);
                }
            }
            _ => {}
        }
    }
}

fn collect_predetermined_private_names<F>(
    stmts: &[SpannedStmt],
    names: &mut Vec<(String, Span)>,
    collapse_depth: &mut F,
) where
    F: FnMut(&[OpenMpClause]) -> i64,
{
    for stmt in stmts {
        match &stmt.node {
            Stmt::DoLoop { var, body, .. } => {
                if let Some(var) = var {
                    names.push((var.to_ascii_lowercase(), stmt.span));
                }
                collect_predetermined_private_names(body, names, collapse_depth);
            }
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_predetermined_private_names(then_body, names, collapse_depth);
                for (_, body) in else_ifs {
                    collect_predetermined_private_names(body, names, collapse_depth);
                }
                if let Some(body) = else_body {
                    collect_predetermined_private_names(body, names, collapse_depth);
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::ForallStmt { stmt: action, .. }
            | Stmt::Labeled { stmt: action, .. } => {
                collect_predetermined_private_names(
                    std::slice::from_ref(action.as_ref()),
                    names,
                    collapse_depth,
                );
            }
            Stmt::DoWhile { body, .. }
            | Stmt::DoConcurrent { body, .. }
            | Stmt::Block { body, .. }
            | Stmt::Associate { body, .. }
            | Stmt::ForallConstruct { body, .. } => {
                collect_predetermined_private_names(body, names, collapse_depth);
            }
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    collect_predetermined_private_names(&case.body, names, collapse_depth);
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        TypeGuard::TypeIs { body, .. }
                        | TypeGuard::ClassIs { body, .. }
                        | TypeGuard::ClassDefault { body } => body,
                    };
                    collect_predetermined_private_names(body, names, collapse_depth);
                }
            }
            Stmt::SelectRank { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        RankGuard::Rank { body, .. }
                        | RankGuard::RankStar { body }
                        | RankGuard::RankDefault { body } => body,
                    };
                    collect_predetermined_private_names(body, names, collapse_depth);
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                collect_predetermined_private_names(body, names, collapse_depth);
                for (_, body) in elsewhere {
                    collect_predetermined_private_names(body, names, collapse_depth);
                }
            }
            // A standalone worksharing DO uses the current parallel team's
            // implicit tasks, so its associated loop variable must be
            // available as private state in that enclosing callback. A
            // nested PARALLEL or combined PARALLEL DO owns a distinct data
            // environment and must not affect this region's classification.
            Stmt::OpenMp(OpenMpConstruct::Do { loop_stmt, .. }) => {
                collect_predetermined_private_names(
                    std::slice::from_ref(loop_stmt.as_ref()),
                    names,
                    collapse_depth,
                )
            }
            Stmt::OpenMp(OpenMpConstruct::Loop { clauses, loop_stmt }) => {
                let explicitly_private: HashSet<_> = clauses
                    .iter()
                    .filter_map(|clause| match clause {
                        OpenMpClause::Private(variables) => Some(variables.as_slice()),
                        _ => None,
                    })
                    .flatten()
                    .map(|name| name.to_ascii_lowercase())
                    .collect();
                let Stmt::DoLoop {
                    var: Some(outer),
                    body,
                    ..
                } = &loop_stmt.node
                else {
                    collect_predetermined_private_names(
                        std::slice::from_ref(loop_stmt.as_ref()),
                        names,
                        collapse_depth,
                    );
                    continue;
                };
                if explicitly_private.contains(&outer.to_ascii_lowercase()) {
                    names.push((outer.to_ascii_lowercase(), loop_stmt.span));
                }
                if collapse_depth(clauses) == 2 {
                    let Some(inner_stmt) = body.first() else {
                        continue;
                    };
                    let Stmt::DoLoop {
                        var: Some(inner),
                        body: inner_body,
                        ..
                    } = &inner_stmt.node
                    else {
                        collect_predetermined_private_names(body, names, collapse_depth);
                        continue;
                    };
                    if explicitly_private.contains(&inner.to_ascii_lowercase()) {
                        names.push((inner.to_ascii_lowercase(), inner_stmt.span));
                    }
                    collect_predetermined_private_names(inner_body, names, collapse_depth);
                } else {
                    collect_predetermined_private_names(body, names, collapse_depth);
                }
            }
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
                && ctx
                    .lookup_lexical(&name)
                    .is_some_and(|symbol| callable_reference_needs_capture(ctx.st, symbol, &name)));
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
        if symbol.attrs.openmp_threadprivate {
            continue;
        }
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
        } else {
            match clause_info.default_data_sharing {
                Some(OpenMpDefault::Private) => {
                    validate_private_object(ctx, &name, span, "DEFAULT(PRIVATE)")
                }
                Some(OpenMpDefault::None) => ctx.error(
                    span,
                    format!(
                        "OpenMP DEFAULT(NONE) variable '{}' must appear in a data-sharing clause",
                        name
                    ),
                ),
                Some(OpenMpDefault::Shared) | Some(OpenMpDefault::FirstPrivate) | None => {
                    validate_shared_object(ctx, &name, span)
                }
            }
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
    if symbol.attrs.openmp_threadprivate {
        ctx.error(
            span,
            format!(
                "OpenMP THREADPRIVATE variable '{}' may not appear in a SHARED clause",
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
        let supported_numeric_or_logical = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Integer { .. })
                | Some(TypeInfo::Real { .. })
                | Some(TypeInfo::DoublePrecision)
                | Some(TypeInfo::Logical { .. })
        ) || matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Complex { kind })
                if matches!(kind.unwrap_or_else(crate::driver::defaults::default_real_kind), 4 | 8)
        );
        let supported_character = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Character {
                len: Some(_),
                kind,
            }) if kind.unwrap_or(1) == 1
        );
        let supported_derived = matches!(symbol.type_info.as_ref(), Some(TypeInfo::Derived(_)));
        if !supported_numeric_or_logical && !supported_character && !supported_derived {
            ctx.error(
                span,
                format!(
                    "OpenMP PARALLEL shared array '{}' must currently have INTEGER, REAL, DOUBLE PRECISION, COMPLEX, LOGICAL, fixed-length default-kind CHARACTER, or nonpolymorphic derived-type elements",
                    name
                ),
            );
        }
        return;
    }
    let supported_numeric_or_logical = matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Integer { .. })
            | Some(TypeInfo::Real { .. })
            | Some(TypeInfo::DoublePrecision)
            | Some(TypeInfo::Logical { .. })
    ) || matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Complex { kind })
            if matches!(kind.unwrap_or_else(crate::driver::defaults::default_real_kind), 4 | 8)
    );
    let supported_character = matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Character {
            len: Some(_),
            kind,
        }) if kind.unwrap_or(1) == 1
    ) || (symbol.attrs.allocatable
        && matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Character { kind, .. }) if kind.unwrap_or(1) == 1
        ));
    if !supported_numeric_or_logical && !supported_character {
        ctx.error(
            span,
            format!(
                "OpenMP PARALLEL shared variable '{}' must currently be a scalar INTEGER, REAL, DOUBLE PRECISION, COMPLEX, LOGICAL, fixed-length default-kind CHARACTER, or default-kind CHARACTER allocatable",
                name
            ),
        );
    }
}

fn validate_copyin_object(ctx: &mut Ctx<'_>, name: &str, span: Span) {
    let Some(symbol) = ctx.lookup_lexical(name) else {
        ctx.error(
            span,
            format!("OpenMP COPYIN variable '{name}' does not resolve to a visible data object"),
        );
        return;
    };
    if symbol.kind != SymbolKind::Variable {
        ctx.error(
            span,
            format!("OpenMP COPYIN list item '{name}' must be a variable"),
        );
        return;
    }
    if !symbol.attrs.openmp_threadprivate {
        ctx.error(
            span,
            format!("OpenMP COPYIN variable '{name}' must be THREADPRIVATE"),
        );
        return;
    }
    if symbol.attrs.pointer {
        ctx.error(
            span,
            format!(
                "OpenMP COPYIN pointer variable '{name}' is recognized but not yet implemented"
            ),
        );
        return;
    }
    if symbol.attrs.allocatable
        || !matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Integer { .. })
                | Some(TypeInfo::Real { .. })
                | Some(TypeInfo::DoublePrecision)
                | Some(TypeInfo::Complex { .. })
                | Some(TypeInfo::Logical { .. })
        )
    {
        ctx.error(
            span,
            format!(
                "OpenMP COPYIN variable '{name}' currently requires a nonallocatable, nonpointer intrinsic numeric or LOGICAL THREADPRIVATE variable"
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
    if symbol.attrs.openmp_threadprivate {
        ctx.error(
            span,
            format!(
                "OpenMP THREADPRIVATE variable '{}' may not appear in a {} clause",
                name, clause
            ),
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
    if symbol.attrs.target {
        ctx.error(
            span,
            format!(
                "OpenMP {} pointer or target variable '{}' is recognized but not yet implemented",
                clause, name
            ),
        );
        return;
    }
    if symbol.attrs.pointer {
        let scalar = symbol.attrs.array_spec.is_empty();
        let deferred_shape_array = !scalar
            && symbol
                .attrs
                .array_spec
                .iter()
                .all(|spec| matches!(spec, crate::ast::decl::ArraySpec::Deferred));
        if !scalar && !deferred_shape_array {
            ctx.error(
                span,
                format!(
                    "OpenMP {} non-deferred-shape pointer array '{}' is recognized but not yet implemented",
                    clause, name
                ),
            );
            return;
        }
        if clause != "FIRSTPRIVATE" && symbol.attrs.intent == Some(Intent::In) {
            ctx.error(
                span,
                format!(
                    "OpenMP {} pointer '{}' may not have INTENT(IN)",
                    clause, name
                ),
            );
            return;
        }
        let supported_numeric_or_logical = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Integer { .. })
                | Some(TypeInfo::Real { .. })
                | Some(TypeInfo::DoublePrecision)
                | Some(TypeInfo::Logical { .. })
        );
        let supported_character_pointer = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Character {
                len: Some(_),
                kind,
            }) if kind.unwrap_or(1) == 1
        );
        if !supported_numeric_or_logical && !supported_character_pointer {
            let supported_types = if scalar {
                "INTEGER, REAL, DOUBLE PRECISION, LOGICAL, or fixed-length default-kind CHARACTER type"
            } else {
                "INTEGER, REAL, DOUBLE PRECISION, LOGICAL, or fixed-length default-kind CHARACTER elements"
            };
            ctx.error(
                span,
                format!(
                    "OpenMP {} {} '{}' must currently have {}",
                    clause,
                    if scalar { "pointer" } else { "pointer array" },
                    name,
                    supported_types,
                ),
            );
        }
        return;
    }
    if symbol.attrs.allocatable {
        let scalar = symbol.attrs.array_spec.is_empty();
        let deferred_shape_array = !scalar
            && symbol
                .attrs
                .array_spec
                .iter()
                .all(|spec| matches!(spec, crate::ast::decl::ArraySpec::Deferred));
        if !scalar && !deferred_shape_array {
            ctx.error(
                span,
                format!(
                    "OpenMP {} non-deferred-shape allocatable array '{}' is recognized but not yet implemented",
                    clause, name
                ),
            );
            return;
        }
        let supported_numeric_or_logical = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Integer { .. })
                | Some(TypeInfo::Real { .. })
                | Some(TypeInfo::DoublePrecision)
                | Some(TypeInfo::Logical { .. })
        );
        let supported_character_allocatable = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Character { len, kind })
                if kind.unwrap_or(1) == 1 && (scalar || len.is_some())
        );
        if !supported_numeric_or_logical && !supported_character_allocatable {
            let supported_types = if scalar {
                "INTEGER, REAL, DOUBLE PRECISION, LOGICAL, or default-kind CHARACTER type"
            } else {
                "INTEGER, REAL, DOUBLE PRECISION, LOGICAL, or fixed-length default-kind CHARACTER elements"
            };
            ctx.error(
                span,
                format!(
                    "OpenMP {} {} '{}' must currently have {}",
                    clause,
                    if scalar {
                        "allocatable"
                    } else {
                        "allocatable array"
                    },
                    name,
                    supported_types,
                ),
            );
        }
        return;
    }
    let private_explicit_shape_dummy = is_dummy
        && clause == "PRIVATE"
        && !symbol.attrs.array_spec.is_empty()
        && symbol
            .attrs
            .array_spec
            .iter()
            .all(|spec| matches!(spec, crate::ast::decl::ArraySpec::Explicit { .. }));
    if is_dummy && !symbol.attrs.array_spec.is_empty() && !private_explicit_shape_dummy {
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
        if !private_explicit_shape_dummy
            && symbol
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
        let supported_numeric_or_logical = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Integer { .. })
                | Some(TypeInfo::Real { .. })
                | Some(TypeInfo::DoublePrecision)
                | Some(TypeInfo::Logical { .. })
        ) || matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Complex { kind })
                if matches!(kind.unwrap_or_else(crate::driver::defaults::default_real_kind), 4 | 8)
        );
        let supported_character = matches!(
            symbol.type_info.as_ref(),
            Some(TypeInfo::Character {
                len: Some(_),
                kind,
            }) if kind.unwrap_or(1) == 1
        );
        if !supported_numeric_or_logical && !supported_character {
            ctx.error(
                span,
                format!(
                    "OpenMP {} array '{}' must currently have INTEGER, REAL, DOUBLE PRECISION, COMPLEX, LOGICAL, or fixed-length default-kind CHARACTER elements",
                    clause, name
                ),
            );
        }
        return;
    }
    if let Some(TypeInfo::Derived(type_name)) = symbol.type_info.as_ref() {
        if clause != "PRIVATE" && clause != "FIRSTPRIVATE" {
            ctx.error(
                span,
                format!(
                    "OpenMP {} derived-type variable '{}' is recognized but not yet implemented",
                    clause, name
                ),
            );
            return;
        }
        let layout = ctx.type_layouts.and_then(|layouts| {
            layouts
                .get_for_scope(ctx.scope_id, type_name)
                .or_else(|| layouts.get_for_scope(symbol.scope, type_name))
                .or_else(|| layouts.get(type_name))
        });
        let Some(layout) = layout else {
            ctx.error(
                span,
                format!(
                    "OpenMP {} derived-type variable '{}' has no available type layout",
                    clause, name
                ),
            );
            return;
        };
        if clause == "PRIVATE"
            && ctx.type_layouts.is_some_and(|layouts| {
                derived_layout_has_polymorphic_allocatable_components(
                    layouts,
                    layout,
                    &mut HashSet::new(),
                )
            })
        {
            ctx.error(
                span,
                format!(
                    "OpenMP {} derived-type variable '{}' with polymorphic allocatable components is recognized but not yet implemented",
                    clause, name
                ),
            );
            return;
        }
        if clause == "FIRSTPRIVATE" && !layout.bound_proc_candidates("assignment(=)").is_empty() {
            ctx.error(
                span,
                format!(
                    "OpenMP FIRSTPRIVATE derived-type variable '{}' with type-bound defined assignment is recognized but not yet implemented",
                    name
                ),
            );
        }
        return;
    }
    let supported_numeric_or_logical = matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Integer { .. })
            | Some(TypeInfo::Real { .. })
            | Some(TypeInfo::DoublePrecision)
            | Some(TypeInfo::Logical { .. })
    );
    let supported_character = matches!(
        symbol.type_info.as_ref(),
        Some(TypeInfo::Character {
            len: Some(_),
            kind,
        }) if kind.unwrap_or(1) == 1
    );
    if !supported_numeric_or_logical && !supported_character {
        ctx.error(
            span,
            format!(
                "OpenMP {} variable '{}' must currently be a scalar INTEGER, REAL, DOUBLE PRECISION, LOGICAL, or fixed-length default-kind CHARACTER",
                clause, name
            ),
        );
    }
}

fn derived_layout_has_polymorphic_allocatable_components(
    layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    layout: &crate::sema::type_layout::TypeLayout,
    active: &mut HashSet<String>,
) -> bool {
    let key = layouts.canonical_key_for_layout(layout);
    if !active.insert(key.clone()) {
        return false;
    }
    let has_unsupported = layout.fields.iter().any(|field| {
        if field.allocatable
            && matches!(
                field.type_info,
                TypeInfo::Class(_) | TypeInfo::ClassStar | TypeInfo::TypeStar
            )
        {
            return true;
        }
        if field.pointer {
            return false;
        }
        let TypeInfo::Derived(type_name) = &field.type_info else {
            return false;
        };
        layouts
            .get_related(layout, type_name)
            .is_some_and(|nested| {
                derived_layout_has_polymorphic_allocatable_components(layouts, nested, active)
            })
    });
    active.remove(&key);
    has_unsupported
}

fn validate_structured_block(ctx: &mut Ctx<'_>, stmts: &[SpannedStmt]) {
    let mut internal_labels = HashSet::new();
    collect_structured_block_labels(stmts, &mut internal_labels);
    validate_structured_block_with_cycle_targets(ctx, stmts, &mut Vec::new(), &internal_labels);
}

fn validate_structured_block_with_cycle_targets(
    ctx: &mut Ctx<'_>,
    stmts: &[SpannedStmt],
    cycle_targets: &mut Vec<Option<String>>,
    internal_labels: &HashSet<u64>,
) {
    for stmt in stmts {
        match &stmt.node {
            Stmt::Return { .. } => reject_transfer(ctx, stmt.span, "RETURN"),
            Stmt::Goto { label } if !internal_labels.contains(label) => {
                reject_external_branch(ctx, stmt.span, "GOTO")
            }
            Stmt::ComputedGoto { labels, .. }
                if labels.iter().any(|label| !internal_labels.contains(label)) =>
            {
                reject_external_branch(ctx, stmt.span, "computed GOTO")
            }
            Stmt::ArithmeticIf { neg, zero, pos, .. }
                if [neg, zero, pos]
                    .into_iter()
                    .any(|label| !internal_labels.contains(label)) =>
            {
                reject_external_branch(ctx, stmt.span, "arithmetic IF")
            }
            Stmt::Exit { .. } => reject_transfer(ctx, stmt.span, "EXIT"),
            Stmt::Cycle { name } => {
                let target_is_internal = match name {
                    Some(name) => cycle_targets.iter().rev().any(|target| {
                        target
                            .as_deref()
                            .is_some_and(|target| target.eq_ignore_ascii_case(name))
                    }),
                    None => !cycle_targets.is_empty(),
                };
                if !target_is_internal {
                    reject_transfer(ctx, stmt.span, "CYCLE");
                }
            }
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
                validate_structured_block_with_cycle_targets(
                    ctx,
                    then_body,
                    cycle_targets,
                    internal_labels,
                );
                for (_, body) in else_ifs {
                    validate_structured_block_with_cycle_targets(
                        ctx,
                        body,
                        cycle_targets,
                        internal_labels,
                    );
                }
                if let Some(body) = else_body {
                    validate_structured_block_with_cycle_targets(
                        ctx,
                        body,
                        cycle_targets,
                        internal_labels,
                    );
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::ForallStmt { stmt: action, .. }
            | Stmt::Labeled { stmt: action, .. } => {
                validate_structured_block_with_cycle_targets(
                    ctx,
                    std::slice::from_ref(action.as_ref()),
                    cycle_targets,
                    internal_labels,
                );
            }
            Stmt::DoLoop { name, body, .. }
            | Stmt::DoWhile { name, body, .. }
            | Stmt::DoConcurrent { name, body, .. } => {
                cycle_targets.push(name.as_deref().map(str::to_ascii_lowercase));
                validate_structured_block_with_cycle_targets(
                    ctx,
                    body,
                    cycle_targets,
                    internal_labels,
                );
                cycle_targets.pop();
            }
            Stmt::Block { body, .. }
            | Stmt::Associate { body, .. }
            | Stmt::ForallConstruct { body, .. } => validate_structured_block_with_cycle_targets(
                ctx,
                body,
                cycle_targets,
                internal_labels,
            ),
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    validate_structured_block_with_cycle_targets(
                        ctx,
                        &case.body,
                        cycle_targets,
                        internal_labels,
                    );
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        TypeGuard::TypeIs { body, .. }
                        | TypeGuard::ClassIs { body, .. }
                        | TypeGuard::ClassDefault { body } => body,
                    };
                    validate_structured_block_with_cycle_targets(
                        ctx,
                        body,
                        cycle_targets,
                        internal_labels,
                    );
                }
            }
            Stmt::SelectRank { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        RankGuard::Rank { body, .. }
                        | RankGuard::RankStar { body }
                        | RankGuard::RankDefault { body } => body,
                    };
                    validate_structured_block_with_cycle_targets(
                        ctx,
                        body,
                        cycle_targets,
                        internal_labels,
                    );
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                validate_structured_block_with_cycle_targets(
                    ctx,
                    body,
                    cycle_targets,
                    internal_labels,
                );
                for (_, body) in elsewhere {
                    validate_structured_block_with_cycle_targets(
                        ctx,
                        body,
                        cycle_targets,
                        internal_labels,
                    );
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

fn collect_structured_block_labels(stmts: &[SpannedStmt], labels: &mut HashSet<u64>) {
    for stmt in stmts {
        match &stmt.node {
            Stmt::Continue { label: Some(label) } => {
                labels.insert(*label);
            }
            Stmt::Labeled { label, stmt } => {
                labels.insert(*label);
                collect_structured_block_labels(std::slice::from_ref(stmt.as_ref()), labels);
            }
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_structured_block_labels(then_body, labels);
                for (_, body) in else_ifs {
                    collect_structured_block_labels(body, labels);
                }
                if let Some(body) = else_body {
                    collect_structured_block_labels(body, labels);
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::ForallStmt { stmt: action, .. } => {
                collect_structured_block_labels(std::slice::from_ref(action.as_ref()), labels)
            }
            Stmt::DoLoop { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::DoConcurrent { body, .. }
            | Stmt::Block { body, .. }
            | Stmt::Associate { body, .. }
            | Stmt::ForallConstruct { body, .. } => collect_structured_block_labels(body, labels),
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    collect_structured_block_labels(&case.body, labels);
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        TypeGuard::TypeIs { body, .. }
                        | TypeGuard::ClassIs { body, .. }
                        | TypeGuard::ClassDefault { body } => body,
                    };
                    collect_structured_block_labels(body, labels);
                }
            }
            Stmt::SelectRank { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        RankGuard::Rank { body, .. }
                        | RankGuard::RankStar { body }
                        | RankGuard::RankDefault { body } => body,
                    };
                    collect_structured_block_labels(body, labels);
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                collect_structured_block_labels(body, labels);
                for (_, body) in elsewhere {
                    collect_structured_block_labels(body, labels);
                }
            }
            // A nested OpenMP construct owns a distinct structured block.
            Stmt::OpenMp(_) => {}
            _ => {}
        }
    }
}

fn reject_transfer(ctx: &mut Ctx<'_>, span: Span, statement: &str) {
    ctx.error(
        span,
        format!(
            "{} is not yet supported inside an OpenMP structured block",
            statement
        ),
    );
}

fn reject_external_branch(ctx: &mut Ctx<'_>, span: Span, statement: &str) {
    ctx.error(
        span,
        format!("{statement} may not leave an OpenMP structured block"),
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
            "I/O ERR=/END=/EOR= transfer is not yet supported inside an OpenMP structured block",
        );
    }
}
