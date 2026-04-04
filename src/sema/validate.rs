//! Semantic validation — checks that go beyond type checking.
//!
//! Allocatable/pointer semantics, intent enforcement, pure/elemental
//! constraints, label validation, and standard conformance. Runs after
//! symbol resolution (resolve.rs) and type checking (types.rs).

use crate::ast::unit::*;
use crate::ast::stmt::*;
use crate::ast::expr::Expr;
use crate::ast::decl::{Decl, Attribute, TypeAttr};
use crate::lexer::Span;
use super::symtab::*;

/// Fortran standard level for --std= conformance checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FortranStandard {
    F77,
    F90,
    F95,
    F2003,
    F2008,
    F2018,
    F2023,
}

impl FortranStandard {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "f77" | "fortran77" => Some(Self::F77),
            "f90" | "fortran90" => Some(Self::F90),
            "f95" | "fortran95" => Some(Self::F95),
            "f2003" | "fortran2003" => Some(Self::F2003),
            "f2008" | "fortran2008" => Some(Self::F2008),
            "f2018" | "fortran2018" => Some(Self::F2018),
            "f2023" | "fortran2023" => Some(Self::F2023),
            _ => None,
        }
    }
}

/// A diagnostic produced by validation.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub span: Span,
    pub kind: DiagKind,
    pub msg: String,
}

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagKind {
    Error,
    Warning,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self.kind {
            DiagKind::Error => "error",
            DiagKind::Warning => "warning",
        };
        write!(f, "{}:{}: {}: {}", self.span.start.line, self.span.start.col, label, self.msg)
    }
}

/// Validation context — accumulates diagnostics while walking the AST.
struct Ctx<'a> {
    st: &'a SymbolTable,
    diags: Vec<Diagnostic>,
    /// Current scope ID for symbol lookups.
    scope_id: ScopeId,
    /// Are we inside a pure procedure?
    in_pure: bool,
    /// Are we inside an elemental procedure?
    in_elemental: bool,
    /// Target standard for conformance checking (None = allow everything).
    std: Option<FortranStandard>,
    /// Labels defined in the current scope.
    labels_defined: Vec<u64>,
    /// Labels referenced (GOTO targets) in the current scope.
    labels_referenced: Vec<(u64, Span)>,
}

impl<'a> Ctx<'a> {
    fn new(st: &'a SymbolTable, std: Option<FortranStandard>) -> Self {
        Self {
            st,
            diags: Vec::new(),
            scope_id: 0,
            in_pure: false,
            in_elemental: false,
            std,
            labels_defined: Vec::new(),
            labels_referenced: Vec::new(),
        }
    }

    /// Emit an error if a feature requires a newer standard than selected.
    fn require_std(&mut self, span: Span, min: FortranStandard, feature: &str) {
        if let Some(selected) = self.std {
            if selected < min {
                self.error(span, format!("{} requires --std={:?} or later", feature, min));
            }
        }
    }

    /// Look up a symbol in the current validation scope.
    fn lookup(&self, name: &str) -> Option<&Symbol> {
        self.st.lookup_in(self.scope_id, name)
    }

    fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic { span, kind: DiagKind::Error, msg: msg.into() });
    }

    fn warning(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic { span, kind: DiagKind::Warning, msg: msg.into() });
    }
}

/// Validate a parsed and resolved file. Returns diagnostics (errors and warnings).
pub fn validate_file(units: &[SpannedUnit], st: &SymbolTable) -> Vec<Diagnostic> {
    validate_file_with_std(units, st, None)
}

/// Validate with a specific standard level for conformance checking.
pub fn validate_file_with_std(
    units: &[SpannedUnit],
    st: &SymbolTable,
    std: Option<FortranStandard>,
) -> Vec<Diagnostic> {
    let mut ctx = Ctx::new(st, std);
    for unit in units {
        validate_unit(&mut ctx, unit);
    }
    ctx.diags
}

/// Find the scope ID for a program unit, preferring children of `parent_scope`.
/// This resolves ambiguity when multiple scopes share a name (e.g., a module
/// subroutine and a CONTAINS subroutine with the same name).
fn find_scope_for_unit(st: &SymbolTable, unit: &ProgramUnit, parent_scope: ScopeId) -> Option<ScopeId> {
    #[allow(clippy::type_complexity)]
    let (kind_matcher, _name): (Box<dyn Fn(&ScopeKind) -> bool>, Option<String>) = match unit {
        ProgramUnit::Program { name, .. } => {
            let target = name.clone().unwrap_or_else(|| "<main>".into());
            (Box::new(move |k| matches!(k, ScopeKind::Program(ref n) if n == &target)), None)
        }
        ProgramUnit::Module { name, .. } => {
            let n = name.clone();
            (Box::new(move |k| matches!(k, ScopeKind::Module(ref m) if m.eq_ignore_ascii_case(&n))), Some(name.clone()))
        }
        ProgramUnit::Subroutine { name, .. } => {
            let n = name.clone();
            (Box::new(move |k| matches!(k, ScopeKind::Subroutine(ref m) if m.eq_ignore_ascii_case(&n))), Some(name.clone()))
        }
        ProgramUnit::Function { name, .. } => {
            let n = name.clone();
            (Box::new(move |k| matches!(k, ScopeKind::Function(ref m) if m.eq_ignore_ascii_case(&n))), Some(name.clone()))
        }
        ProgramUnit::BlockData { name, .. } => {
            let target = name.clone().unwrap_or_else(|| "<block_data>".into());
            (Box::new(move |k| matches!(k, ScopeKind::Program(ref n) if n == &target)), None)
        }
        _ => return None,
    };

    // Prefer a child of the current parent scope.
    let child = st.scopes.iter().find(|s| {
        s.parent == Some(parent_scope) && kind_matcher(&s.kind)
    });
    if let Some(s) = child {
        return Some(s.id);
    }

    // Fall back to any matching scope.
    st.scopes.iter().find(|s| kind_matcher(&s.kind)).map(|s| s.id)
}

fn validate_unit(ctx: &mut Ctx, unit: &SpannedUnit) {
    let saved_scope = ctx.scope_id;
    if let Some(scope_id) = find_scope_for_unit(ctx.st, &unit.node, ctx.scope_id) {
        ctx.scope_id = scope_id;
    }

    match &unit.node {
        ProgramUnit::Program { decls, body, contains, .. } => {
            validate_decls(ctx, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            for sub in contains {
                validate_unit(ctx, sub);
            }
        }
        ProgramUnit::Module { decls, contains, .. } => {
            validate_decls(ctx, decls);
            for sub in contains {
                validate_unit(ctx, sub);
            }
        }
        ProgramUnit::Subroutine { prefix, decls, body, contains, args, .. } => {
            let saved_pure = ctx.in_pure;
            let saved_elemental = ctx.in_elemental;
            ctx.in_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            ctx.in_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            if ctx.in_elemental {
                ctx.in_pure = true;
            }

            if ctx.in_elemental {
                validate_elemental_args(ctx, args, decls, unit.span);
            }

            validate_decls(ctx, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            for sub in contains {
                validate_unit(ctx, sub);
            }
            ctx.in_pure = saved_pure;
            ctx.in_elemental = saved_elemental;
        }
        ProgramUnit::Function { prefix, decls, body, contains, args, .. } => {
            let saved_pure = ctx.in_pure;
            let saved_elemental = ctx.in_elemental;
            ctx.in_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            ctx.in_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            if ctx.in_elemental {
                ctx.in_pure = true;
            }

            if ctx.in_elemental {
                validate_elemental_args(ctx, args, decls, unit.span);
            }

            validate_decls(ctx, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            for sub in contains {
                validate_unit(ctx, sub);
            }
            ctx.in_pure = saved_pure;
            ctx.in_elemental = saved_elemental;
        }
        ProgramUnit::Submodule { decls, contains, .. } => {
            validate_decls(ctx, decls);
            for sub in contains {
                validate_unit(ctx, sub);
            }
        }
        ProgramUnit::BlockData { decls, .. } => {
            validate_decls(ctx, decls);
        }
        ProgramUnit::InterfaceBlock { name, is_abstract, bodies } => {
            // Validate defined operator interfaces.
            if let Some(ref iface_name) = name {
                if is_operator_interface(iface_name) {
                    validate_operator_interface(ctx, iface_name, bodies, unit.span);
                }
            }
            // Abstract interfaces cannot have MODULE PROCEDURE.
            if *is_abstract {
                for body in bodies {
                    if let InterfaceBody::ModuleProcedure(names) = body {
                        if !names.is_empty() {
                            ctx.error(unit.span, "abstract interface cannot contain MODULE PROCEDURE statements");
                        }
                    }
                }
            }
            for body in bodies {
                if let InterfaceBody::Subprogram(sub) = body {
                    validate_unit(ctx, sub);
                }
            }
        }
    }

    ctx.scope_id = saved_scope;
}

// ---- Declaration validation ----

fn validate_decls(ctx: &mut Ctx, decls: &[crate::ast::decl::SpannedDecl]) {
    for decl in decls {
        if let Decl::TypeDecl { attrs, entities, type_spec, .. } = &decl.node {
            let has_alloc = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
            let has_pointer = attrs.iter().any(|a| matches!(a, Attribute::Pointer));

            // Deferred-length character must be allocatable or pointer.
            if let crate::ast::decl::TypeSpec::Character(Some(sel)) = type_spec {
                if let Some(crate::ast::decl::LenSpec::Colon) = &sel.len {
                    if !has_alloc && !has_pointer {
                        ctx.error(decl.span, "deferred-length character (len=:) requires allocatable or pointer attribute");
                    }
                }
            }

            // Allocatable + pointer is forbidden.
            if has_alloc && has_pointer {
                ctx.error(decl.span, "a variable cannot be both allocatable and pointer");
            }

            // Parameter with allocatable/pointer is forbidden.
            let has_param = attrs.iter().any(|a| matches!(a, Attribute::Parameter));
            if has_param && has_alloc {
                ctx.error(decl.span, "a named constant (parameter) cannot be allocatable");
            }
            if has_param && has_pointer {
                ctx.error(decl.span, "a named constant (parameter) cannot be a pointer");
            }

            // Pure/elemental: SAVE is forbidden.
            if ctx.in_pure {
                let has_save = attrs.iter().any(|a| matches!(a, Attribute::Save));
                if has_save {
                    ctx.error(decl.span, "SAVE attribute not allowed in pure procedure");
                }
            }

            let _ = entities; // entities checked individually if needed
        }

        // Derived type definition validation.
        if let Decl::DerivedTypeDef { name, attrs: type_attrs, type_bound_procs, components, .. } = &decl.node {
            validate_derived_type(ctx, name, type_attrs, type_bound_procs, components, decl.span);
        }
    }
}

// ---- Statement validation ----

fn validate_stmts(ctx: &mut Ctx, stmts: &[SpannedStmt]) {
    for stmt in stmts {
        validate_stmt(ctx, stmt);
    }
}

fn validate_stmt(ctx: &mut Ctx, stmt: &SpannedStmt) {
    match &stmt.node {
        // ---- Assignment ----
        Stmt::Assignment { target, .. } => {
            validate_assignment_target(ctx, target, stmt.span);
        }
        Stmt::PointerAssignment { target, value, .. } => {
            validate_pointer_assignment(ctx, target, value, stmt.span);
        }

        // ---- Allocate / Deallocate ----
        Stmt::Allocate { items, .. } => {
            for item in items {
                validate_allocatable_item(ctx, item, "allocate");
            }
        }
        Stmt::Deallocate { items, .. } => {
            for item in items {
                validate_allocatable_item(ctx, item, "deallocate");
            }
        }

        // ---- I/O in pure ----
        Stmt::Write { .. } | Stmt::Read { .. } | Stmt::Print { .. } |
        Stmt::Open { .. } | Stmt::Close { .. } | Stmt::Inquire { .. } |
        Stmt::Rewind { .. } | Stmt::Backspace { .. } | Stmt::Endfile { .. } |
        Stmt::Flush { .. } | Stmt::Wait { .. } => {
            if ctx.in_pure {
                ctx.error(stmt.span, "I/O statement not allowed in pure procedure");
            }
        }

        // ---- STOP in pure ----
        Stmt::Stop { .. } => {
            if ctx.in_pure {
                ctx.error(stmt.span, "STOP not allowed in pure procedure");
            }
        }
        Stmt::ErrorStop { .. } => {
            if ctx.in_pure {
                ctx.error(stmt.span, "ERROR STOP not allowed in pure procedure");
            }
            ctx.require_std(stmt.span, FortranStandard::F2008, "ERROR STOP");
        }

        // ---- GOTO / labels ----
        Stmt::Goto { label } => {
            ctx.labels_referenced.push((*label, stmt.span));
        }
        Stmt::ComputedGoto { labels, .. } => {
            for label in labels {
                ctx.labels_referenced.push((*label, stmt.span));
            }
        }
        Stmt::ArithmeticIf { neg, zero, pos, .. } => {
            ctx.labels_referenced.push((*neg, stmt.span));
            ctx.labels_referenced.push((*zero, stmt.span));
            ctx.labels_referenced.push((*pos, stmt.span));
        }
        Stmt::Continue { label: Some(lbl) } => {
            register_label(ctx, *lbl, stmt.span);
        }

        // ---- Control flow — recurse into bodies ----
        Stmt::IfConstruct { then_body, else_ifs, else_body, .. } => {
            validate_stmts(ctx, then_body);
            for (_, body) in else_ifs {
                validate_stmts(ctx, body);
            }
            if let Some(body) = else_body {
                validate_stmts(ctx, body);
            }
        }
        Stmt::IfStmt { action, .. } => validate_stmt(ctx, action),
        Stmt::DoLoop { body, .. } => validate_stmts(ctx, body),
        Stmt::DoWhile { body, .. } => validate_stmts(ctx, body),
        Stmt::DoConcurrent { body, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F2008, "DO CONCURRENT");
            validate_stmts(ctx, body);
        }
        Stmt::SelectCase { cases, .. } => {
            for case in cases {
                validate_stmts(ctx, &case.body);
            }
        }
        Stmt::WhereConstruct { body, elsewhere, .. } => {
            validate_stmts(ctx, body);
            for (_, ebody) in elsewhere {
                validate_stmts(ctx, ebody);
            }
        }
        Stmt::WhereStmt { stmt: inner, .. } => validate_stmt(ctx, inner),
        Stmt::ForallConstruct { body, .. } => validate_stmts(ctx, body),
        Stmt::ForallStmt { stmt: inner, .. } => validate_stmt(ctx, inner),
        Stmt::Block { body, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F2008, "BLOCK construct");
            validate_stmts(ctx, body);
        }
        Stmt::Associate { assocs, body, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F2003, "ASSOCIATE construct");
            validate_associate(ctx, assocs, body, stmt.span);
        }

        // Call in pure: callee must be pure (we check if it's known impure).
        Stmt::Call { callee, args, .. } => {
            if ctx.in_pure {
                validate_pure_call(ctx, callee, stmt.span);
            }
            validate_call_site_intent(ctx, callee, args, stmt.span);
        }

        // Nullify: items must be pointers.
        Stmt::Nullify { items } => {
            for item in items {
                if let Some(ref name) = extract_base_name(item) {
                    let is_pointer = ctx.lookup(name).map(|s| s.attrs.pointer).unwrap_or(true);
                    if !is_pointer {
                        ctx.error(item.span, format!(
                            "NULLIFY target '{}' must have pointer attribute", name
                        ));
                    }
                }
            }
        }

        // Embedded declarations (e.g., inside BLOCK constructs).
        Stmt::Declaration(decl) => {
            validate_decls(ctx, std::slice::from_ref(decl));
        }

        _ => {}
    }
}

// ---- Specific validation checks ----

/// Check that an assignment target is modifiable (not intent(in), not parameter).
/// Handles component access (x%field) and array elements (a(i)) — the base
/// variable's intent/parameter status applies to all parts.
fn validate_assignment_target(ctx: &mut Ctx, target: &crate::ast::expr::SpannedExpr, span: Span) {
    if let Some(name) = extract_base_name(target) {
        let (is_intent_in, is_parameter) = ctx.lookup(&name)
            .map(|sym| (matches!(sym.attrs.intent, Some(Intent::In)), sym.attrs.parameter))
            .unwrap_or((false, false));
        if is_intent_in {
            ctx.error(span, format!("cannot assign to intent(in) variable '{}'", name));
        }
        if is_parameter {
            ctx.error(span, format!("cannot assign to named constant '{}'", name));
        }
    }
}

/// Validate pointer assignment: LHS must be pointer, RHS must be target/pointer.
fn validate_pointer_assignment(
    ctx: &mut Ctx,
    target: &crate::ast::expr::SpannedExpr,
    value: &crate::ast::expr::SpannedExpr,
    span: Span,
) {
    // For component access (p%ptr_field => x), the final component determines
    // pointer-ness, but we only have the base name. Check the base for now.
    if let Some(name) = extract_base_name(target) {
        let is_pointer = ctx.lookup(&name).map(|s| s.attrs.pointer).unwrap_or(true);
        if !is_pointer {
            ctx.error(span, format!("pointer assignment target '{}' must have pointer attribute", name));
        }
    }

    // RHS must have target attribute or be a pointer (or null()/function call).
    if let Some(name) = extract_base_name(value) {
        // Skip if the value is a function call — could be null() or pointer-valued function.
        if matches!(value.node, Expr::FunctionCall { .. }) {
            return;
        }
        let ok = ctx.lookup(&name).map(|s| s.attrs.target || s.attrs.pointer).unwrap_or(true);
        if !ok {
            ctx.error(span, format!("pointer assignment source '{}' must have target or pointer attribute", name));
        }
    }
}

/// Validate that an ALLOCATE/DEALLOCATE item is allocatable or pointer.
fn validate_allocatable_item(ctx: &mut Ctx, item: &crate::ast::expr::SpannedExpr, stmt_name: &str) {
    let base_name = extract_base_name(item);
    if let Some(ref name) = base_name {
        let ok = ctx.lookup(name)
            .map(|s| s.attrs.allocatable || s.attrs.pointer)
            .unwrap_or(true); // unknown symbol — skip
        if !ok {
            ctx.error(item.span, format!(
                "only allocatable or pointer variables can appear in {}, but '{}' is neither",
                stmt_name.to_uppercase(), name
            ));
        }
    }
}

/// Check if a call in a pure procedure is to a known impure procedure.
/// NOTE: This is a partial check — full validation requires tracking the
/// pure attribute in the symbol table, which isn't done yet. Currently we
/// only catch I/O, STOP, and SAVE violations (in validate_stmt). This stub
/// exists for when symbol-level pure tracking is added.
fn validate_pure_call(_ctx: &mut Ctx, _callee: &crate::ast::expr::SpannedExpr, _span: Span) {
    // Deferred: requires pure/impure attribute on Symbol.
}

/// Validate call-site argument intent constraints.
/// Can't pass a literal, parameter, or expression to intent(out/inout).
fn validate_call_site_intent(
    ctx: &mut Ctx,
    callee: &crate::ast::expr::SpannedExpr,
    args: &[crate::ast::expr::Argument],
    span: Span,
) {
    // Look up the callee to find its dummy argument intents.
    let callee_name = if let Expr::Name { name } = &callee.node { name.clone() } else { return; };

    // For each actual argument, check if it's an lvalue when the dummy requires out/inout.
    // We can only check this if the callee's dummy arg info is in the symbol table.
    // For now, check the simpler case: passing a literal or parameter to ANY subroutine arg.
    for arg in args {
        let actual = match &arg.value {
            crate::ast::expr::SectionSubscript::Element(e) => e,
            _ => continue,
        };
        // Check if actual is a literal (not an lvalue).
        let is_literal = matches!(actual.node,
            Expr::IntegerLiteral { .. } | Expr::RealLiteral { .. } |
            Expr::StringLiteral { .. } | Expr::LogicalLiteral { .. } |
            Expr::ComplexLiteral { .. }
        );
        // Check if actual is a named constant (parameter).
        let is_parameter = if let Some(name) = extract_base_name(actual) {
            ctx.lookup(&name).map(|s| s.attrs.parameter).unwrap_or(false)
        } else { false };

        if is_literal || is_parameter {
            // We can't tell without the callee's interface whether this arg is
            // intent(out/inout). But if the callee IS known and has dummy arg info,
            // we could check. For now, this infrastructure is in place for when
            // we have full interface resolution.
            // Full check deferred until interfaces are tracked in symbol table.
        }
    }
    let _ = callee_name;
    let _ = span;
}

/// Validate elemental procedure arguments are scalar.
fn validate_elemental_args(
    ctx: &mut Ctx,
    args: &[DummyArg],
    decls: &[crate::ast::decl::SpannedDecl],
    span: Span,
) {
    // Elemental: all dummy arguments must be scalar (no dimension attribute).
    for arg in args {
        if let DummyArg::Name(arg_name) = arg {
            for decl in decls {
                if let Decl::TypeDecl { attrs, entities, .. } = &decl.node {
                    for entity in entities {
                        if entity.name.eq_ignore_ascii_case(arg_name) {
                            // Check for dimension attribute or explicit array spec on entity.
                            let has_dimension = attrs.iter().any(|a| matches!(a, Attribute::Dimension(_)));
                            let has_entity_dims = entity.array_spec.is_some();
                            if has_dimension || has_entity_dims {
                                ctx.error(span, format!(
                                    "elemental procedure argument '{}' must be scalar",
                                    arg_name
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Register a label as defined.
fn register_label(ctx: &mut Ctx, label: u64, span: Span) {
    if ctx.labels_defined.contains(&label) {
        ctx.error(span, format!("duplicate label {}", label));
    } else {
        ctx.labels_defined.push(label);
    }
}

/// At the end of a scope, verify all GOTO labels have targets.
fn validate_label_consistency(ctx: &mut Ctx, _scope_span: Span) {
    // Collect errors first to avoid borrow conflict.
    let errors: Vec<(Span, String)> = ctx.labels_referenced.iter()
        .filter(|(label, _)| !ctx.labels_defined.contains(label))
        .map(|(label, span)| (*span, format!("GOTO target label {} not defined in this scope", label)))
        .collect();
    for (span, msg) in errors {
        ctx.error(span, msg);
    }
    ctx.labels_defined.clear();
    ctx.labels_referenced.clear();
}

/// Check if an interface name represents an operator interface.
fn is_operator_interface(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.starts_with("operator(") || lower.starts_with("assignment(")
}

/// Validate a defined operator interface.
fn validate_operator_interface(
    ctx: &mut Ctx,
    iface_name: &str,
    bodies: &[InterfaceBody],
    span: Span,
) {
    let lower = iface_name.to_lowercase();
    let is_assignment = lower.starts_with("assignment(");

    for body in bodies {
        match body {
            InterfaceBody::Subprogram(sub) => {
                match &sub.node {
                    ProgramUnit::Function { args, .. } => {
                        if is_assignment {
                            ctx.error(sub.span, format!(
                                "ASSIGNMENT({}) interface must contain subroutines, not functions",
                                "="
                            ));
                            continue;
                        }
                        // Operator functions: unary = 1 arg, binary = 2 args.
                        let nargs = args.len();
                        if !(1..=2).contains(&nargs) {
                            ctx.error(sub.span, format!(
                                "operator interface function must have 1 or 2 arguments, got {}",
                                nargs
                            ));
                        }
                        // All arguments must be intent(in) — checked by looking at decls.
                        // Deferred: would need to walk the function's decls to check intent.
                    }
                    ProgramUnit::Subroutine { args, .. } => {
                        if !is_assignment {
                            ctx.error(sub.span, "operator interface must contain functions, not subroutines");
                            continue;
                        }
                        // Assignment subroutines must have exactly 2 arguments.
                        if args.len() != 2 {
                            ctx.error(sub.span, format!(
                                "ASSIGNMENT(=) interface subroutine must have 2 arguments, got {}",
                                args.len()
                            ));
                        }
                    }
                    _ => {
                        ctx.error(sub.span, "unexpected program unit in operator interface");
                    }
                }
            }
            InterfaceBody::ModuleProcedure(_) => {
                // Module procedures in operator interface — valid, can't check further
                // without resolving the procedure.
            }
        }
    }
    let _ = span;
}

/// Validate a derived type definition.
fn validate_derived_type(
    ctx: &mut Ctx,
    name: &str,
    type_attrs: &[TypeAttr],
    type_bound_procs: &[crate::ast::decl::TypeBoundProc],
    _components: &[crate::ast::decl::SpannedDecl],
    span: Span,
) {
    let is_abstract = type_attrs.iter().any(|a| matches!(a, TypeAttr::Abstract));

    for tbp in type_bound_procs {
        // Deferred procedures only allowed in abstract types.
        let is_deferred = tbp.attrs.iter().any(|a| a.eq_ignore_ascii_case("deferred"));
        if is_deferred && !is_abstract {
            ctx.error(span, format!(
                "type-bound procedure '{}' is DEFERRED but type '{}' is not ABSTRACT",
                tbp.name, name
            ));
        }

        // PASS and NOPASS are mutually exclusive.
        let has_pass = tbp.attrs.iter().any(|a| {
            let lower = a.to_lowercase();
            lower == "pass" || lower.starts_with("pass(")
        });
        let has_nopass = tbp.attrs.iter().any(|a| a.eq_ignore_ascii_case("nopass"));
        if has_pass && has_nopass {
            ctx.error(span, format!(
                "type-bound procedure '{}' cannot have both PASS and NOPASS",
                tbp.name
            ));
        }

        // Deferred procedures must have an interface (binding).
        if is_deferred && tbp.binding.is_none() {
            ctx.error(span, format!(
                "DEFERRED type-bound procedure '{}' must specify an interface",
                tbp.name
            ));
        }
    }
}

/// Validate ASSOCIATE construct — check that associate names are not empty.
fn validate_associate(
    ctx: &mut Ctx,
    assocs: &[(String, crate::ast::expr::SpannedExpr)],
    body: &[SpannedStmt],
    span: Span,
) {
    for (name, _expr) in assocs {
        if name.is_empty() {
            ctx.error(span, "ASSOCIATE name cannot be empty");
        }
    }
    validate_stmts(ctx, body);
}

/// Extract the base variable name from an expression (handling subscripts and components).
fn extract_base_name(expr: &crate::ast::expr::SpannedExpr) -> Option<String> {
    match &expr.node {
        Expr::Name { name } => Some(name.clone()),
        Expr::FunctionCall { callee, .. } => extract_base_name(callee),
        Expr::ComponentAccess { base, .. } => extract_base_name(base),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::sema::resolve;

    fn validate_source(src: &str) -> Vec<Diagnostic> {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        let st = resolve::resolve_file(&units).unwrap();
        validate_file(&units, &st)
    }

    fn errors_from(src: &str) -> Vec<String> {
        validate_source(src).iter()
            .filter(|d| d.kind == DiagKind::Error)
            .map(|d| d.msg.clone())
            .collect()
    }

    fn errors_with_std(src: &str, std: FortranStandard) -> Vec<String> {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        let st = resolve::resolve_file(&units).unwrap();
        validate_file_with_std(&units, &st, Some(std))
            .iter()
            .filter(|d| d.kind == DiagKind::Error)
            .map(|d| d.msg.clone())
            .collect()
    }

    // ---- Intent enforcement ----

    #[test]
    fn assign_to_intent_in_errors() {
        let errs = errors_from("\
subroutine foo(x)
  real, intent(in) :: x
  x = 1.0
end subroutine
");
        assert!(errs.iter().any(|e| e.contains("intent(in)")));
    }

    #[test]
    fn assign_to_intent_inout_ok() {
        let errs = errors_from("\
subroutine foo(x)
  real, intent(inout) :: x
  x = 1.0
end subroutine
");
        assert!(errs.is_empty());
    }

    #[test]
    fn assign_to_parameter_errors() {
        let errs = errors_from("\
program test
  implicit none
  integer, parameter :: n = 10
  n = 20
end program
");
        assert!(errs.iter().any(|e| e.contains("named constant")));
    }

    // ---- Allocatable / pointer ----

    #[test]
    fn allocate_non_allocatable_errors() {
        let errs = errors_from("\
program test
  implicit none
  real :: x(10)
  allocate(x(20))
end program
");
        assert!(errs.iter().any(|e| e.contains("allocatable or pointer")));
    }

    #[test]
    fn allocate_allocatable_ok() {
        let errs = errors_from("\
program test
  implicit none
  real, allocatable :: x(:)
  allocate(x(10))
end program
");
        assert!(errs.is_empty());
    }

    #[test]
    fn allocatable_and_pointer_forbidden() {
        let errs = errors_from("\
program test
  implicit none
  real, allocatable, pointer :: x
end program
");
        assert!(errs.iter().any(|e| e.contains("both allocatable and pointer")));
    }

    #[test]
    fn parameter_allocatable_forbidden() {
        let errs = errors_from("\
program test
  implicit none
  integer, parameter, allocatable :: x = 10
end program
");
        assert!(errs.iter().any(|e| e.contains("parameter") && e.contains("allocatable")));
    }

    // ---- Pointer assignment ----

    #[test]
    fn pointer_assignment_non_pointer_errors() {
        let errs = errors_from("\
program test
  implicit none
  real :: x
  real, target :: y
  x => y
end program
");
        assert!(errs.iter().any(|e| e.contains("pointer attribute")));
    }

    #[test]
    fn pointer_assignment_non_target_errors() {
        let errs = errors_from("\
program test
  implicit none
  real, pointer :: p
  real :: x
  p => x
end program
");
        assert!(errs.iter().any(|e| e.contains("target or pointer")));
    }

    #[test]
    fn pointer_assignment_ok() {
        let errs = errors_from("\
program test
  implicit none
  real, pointer :: p
  real, target :: x
  p => x
end program
");
        assert!(errs.is_empty());
    }

    // ---- Pure constraints ----

    #[test]
    fn io_in_pure_errors() {
        let errs = errors_from("\
pure subroutine foo(x)
  real, intent(in) :: x
  print *, x
end subroutine
");
        assert!(errs.iter().any(|e| e.contains("I/O") && e.contains("pure")));
    }

    #[test]
    fn stop_in_pure_errors() {
        let errs = errors_from("\
pure function bar(x) result(y)
  real, intent(in) :: x
  real :: y
  y = x
  stop
end function
");
        assert!(errs.iter().any(|e| e.contains("STOP") && e.contains("pure")));
    }

    #[test]
    fn save_in_pure_errors() {
        let errs = errors_from("\
pure subroutine foo(x)
  real, intent(in) :: x
  real, save :: counter
end subroutine
");
        assert!(errs.iter().any(|e| e.contains("SAVE") && e.contains("pure")));
    }

    #[test]
    fn pure_without_violations_ok() {
        let errs = errors_from("\
pure function square(x) result(y)
  real, intent(in) :: x
  real :: y
  y = x * x
end function
");
        assert!(errs.is_empty());
    }

    // ---- Deferred length character ----

    #[test]
    fn deferred_len_without_allocatable_errors() {
        let errs = errors_from("\
program test
  implicit none
  character(len=:) :: s
end program
");
        assert!(errs.iter().any(|e| e.contains("deferred-length")));
    }

    #[test]
    fn deferred_len_with_allocatable_ok() {
        let errs = errors_from("\
program test
  implicit none
  character(len=:), allocatable :: s
end program
");
        assert!(errs.is_empty());
    }

    // ---- Label validation ----
    // Note: the parser does not yet assign labels to statements (labels are
    // separate tokens consumed but not attached). Full GOTO-target validation
    // requires a parser enhancement to track statement labels. The validation
    // infrastructure is in place; these tests verify the diagnostic machinery
    // using the programmatic API directly.

    #[test]
    fn goto_undefined_label_detected() {
        // Test the label validation infrastructure directly.
        use crate::lexer::{Span, Position};
        let st = SymbolTable::new();
        let mut ctx = Ctx::new(&st, None);
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };

        // Reference label 999 but don't define it.
        ctx.labels_referenced.push((999, span));
        validate_label_consistency(&mut ctx, span);
        assert!(ctx.diags.iter().any(|d| d.msg.contains("label 999")));
    }

    #[test]
    fn goto_defined_label_no_error() {
        use crate::lexer::{Span, Position};
        let st = SymbolTable::new();
        let mut ctx = Ctx::new(&st, None);
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };

        ctx.labels_defined.push(10);
        ctx.labels_referenced.push((10, span));
        validate_label_consistency(&mut ctx, span);
        assert!(ctx.diags.is_empty());
    }

    #[test]
    fn duplicate_label_detected() {
        use crate::lexer::{Span, Position};
        let st = SymbolTable::new();
        let mut ctx = Ctx::new(&st, None);
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };

        register_label(&mut ctx, 10, span);
        register_label(&mut ctx, 10, span); // duplicate
        assert!(ctx.diags.iter().any(|d| d.msg.contains("duplicate label")));
    }

    // ---- Valid code produces no errors ----

    #[test]
    fn clean_program_no_errors() {
        let errs = errors_from("\
program test
  implicit none
  integer :: i, n
  real :: x
  n = 10
  do i = 1, n
    x = real(i) * 2.0
  end do
end program
");
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn module_with_subroutine_no_errors() {
        let errs = errors_from("\
module mymod
  implicit none
  integer :: shared
contains
  subroutine update(val)
    integer, intent(in) :: val
    shared = val
  end subroutine
end module
");
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    // ---- Defined operator validation ----
    // Note: the parser doesn't yet support interface blocks in the module
    // specification section (they must appear as top-level units or in
    // CONTAINS). These tests use the validation API directly.

    #[test]
    fn operator_interface_subroutine_errors() {
        // Parse a top-level interface block with operator name.
        let errs = errors_from("\
interface operator(+)
  subroutine bad_add(a, b)
    integer, intent(in) :: a, b
  end subroutine
end interface
");
        assert!(errs.iter().any(|e| e.contains("functions, not subroutines")));
    }

    #[test]
    fn operator_interface_wrong_arg_count() {
        let errs = errors_from("\
interface operator(+)
  function add3(a, b, c) result(r)
    integer, intent(in) :: a, b, c
    integer :: r
  end function
end interface
");
        assert!(errs.iter().any(|e| e.contains("1 or 2 arguments")));
    }

    #[test]
    fn operator_interface_valid_binary() {
        let errs = errors_from("\
interface operator(+)
  function add_vec(a, b) result(c)
    integer, intent(in) :: a, b
    integer :: c
  end function
end interface
");
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn assignment_interface_function_errors() {
        let errs = errors_from("\
interface assignment(=)
  function bad_assign(a, b) result(c)
    integer, intent(in) :: a, b
    integer :: c
  end function
end interface
");
        assert!(errs.iter().any(|e| e.contains("subroutines, not functions")));
    }

    #[test]
    fn assignment_interface_wrong_arg_count() {
        let errs = errors_from("\
interface assignment(=)
  subroutine bad_assign(a, b, c)
    integer, intent(inout) :: a
    integer, intent(in) :: b, c
  end subroutine
end interface
");
        assert!(errs.iter().any(|e| e.contains("2 arguments")));
    }

    // ---- Derived type validation ----

    #[test]
    fn deferred_in_non_abstract_errors() {
        let errs = errors_from("\
module m
  implicit none
  type :: shape
  contains
    procedure, deferred :: area
  end type
end module
");
        assert!(errs.iter().any(|e| e.contains("DEFERRED") && e.contains("not ABSTRACT")));
    }

    #[test]
    fn deferred_in_abstract_ok() {
        let errs = errors_from("\
module m
  implicit none
  type, abstract :: shape
  contains
    procedure, deferred :: area
  end type
end module
");
        // No error for deferred in abstract type (the "must specify interface"
        // error is expected since our parser stores binding as None for simple
        // deferred procedures — that's a parser representation issue).
        assert!(!errs.iter().any(|e| e.contains("not ABSTRACT")));
    }

    #[test]
    fn pass_and_nopass_together_errors() {
        let errs = errors_from("\
module m
  implicit none
  type :: thing
  contains
    procedure, pass, nopass :: method
  end type
end module
");
        assert!(errs.iter().any(|e| e.contains("both PASS and NOPASS")));
    }

    // ---- Standard conformance (--std=) ----

    #[test]
    fn do_concurrent_requires_f2008() {
        let errs = errors_with_std("\
program test
  implicit none
  integer :: i
  do concurrent (i = 1:10)
  end do
end program
", FortranStandard::F95);
        assert!(errs.iter().any(|e| e.contains("DO CONCURRENT") && e.contains("F2008")));
    }

    #[test]
    fn do_concurrent_ok_with_f2008() {
        let errs = errors_with_std("\
program test
  implicit none
  integer :: i
  do concurrent (i = 1:10)
  end do
end program
", FortranStandard::F2008);
        assert!(!errs.iter().any(|e| e.contains("DO CONCURRENT")));
    }

    #[test]
    fn error_stop_requires_f2008() {
        let errs = errors_with_std("\
program test
  implicit none
  error stop
end program
", FortranStandard::F95);
        assert!(errs.iter().any(|e| e.contains("ERROR STOP") && e.contains("F2008")));
    }

    #[test]
    fn block_construct_requires_f2008() {
        let errs = errors_with_std("\
program test
  implicit none
  block
    x = 1
  end block
end program
", FortranStandard::F95);
        assert!(errs.iter().any(|e| e.contains("BLOCK") && e.contains("F2008")));
    }

    #[test]
    fn associate_requires_f2003() {
        let errs = errors_with_std("\
program test
  implicit none
  integer :: n
  n = 10
  associate (m => n)
  end associate
end program
", FortranStandard::F95);
        assert!(errs.iter().any(|e| e.contains("ASSOCIATE") && e.contains("F2003")));
    }

    #[test]
    fn no_std_violations_when_unset() {
        // With no --std= set, everything is allowed.
        let errs = errors_from("\
program test
  implicit none
  integer :: i
  do concurrent (i = 1:10)
  end do
  block
    x = 1
  end block
end program
");
        assert!(!errs.iter().any(|e| e.contains("requires")));
    }

    // ---- Elemental ----

    #[test]
    fn elemental_io_errors() {
        let errs = errors_from("\
elemental subroutine foo(x)
  real, intent(in) :: x
  print *, x
end subroutine
");
        // Elemental implies pure, so I/O is forbidden.
        assert!(errs.iter().any(|e| e.contains("I/O") && e.contains("pure")));
    }
}
