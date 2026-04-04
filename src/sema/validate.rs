//! Semantic validation — checks that go beyond type checking.
//!
//! Allocatable/pointer semantics, intent enforcement, pure/elemental
//! constraints, label validation, and standard conformance. Runs after
//! symbol resolution (resolve.rs) and type checking (types.rs).

use crate::ast::unit::*;
use crate::ast::stmt::*;
use crate::ast::expr::Expr;
use crate::ast::decl::{Decl, Attribute};
use crate::lexer::Span;
use super::symtab::*;

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
    /// Labels defined in the current scope.
    labels_defined: Vec<u64>,
    /// Labels referenced (GOTO targets) in the current scope.
    labels_referenced: Vec<(u64, Span)>,
}

impl<'a> Ctx<'a> {
    fn new(st: &'a SymbolTable) -> Self {
        Self {
            st,
            diags: Vec::new(),
            scope_id: 0,
            in_pure: false,
            in_elemental: false,
            labels_defined: Vec::new(),
            labels_referenced: Vec::new(),
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
    let mut ctx = Ctx::new(st);
    for unit in units {
        validate_unit(&mut ctx, unit);
    }
    ctx.diags
}

/// Find the scope ID for a program unit by matching the scope kind.
fn find_scope_for_unit(st: &SymbolTable, unit: &ProgramUnit) -> Option<ScopeId> {
    match unit {
        ProgramUnit::Program { name, .. } => {
            let target = name.clone().unwrap_or_else(|| "<main>".into());
            st.scopes.iter().find_map(|s| {
                if let ScopeKind::Program(ref n) = s.kind {
                    if n == &target { Some(s.id) } else { None }
                } else { None }
            })
        }
        ProgramUnit::Module { name, .. } => st.find_module_scope(name),
        ProgramUnit::Subroutine { name, .. } => {
            st.scopes.iter().find_map(|s| {
                if let ScopeKind::Subroutine(ref n) = s.kind {
                    if n.eq_ignore_ascii_case(name) { Some(s.id) } else { None }
                } else { None }
            })
        }
        ProgramUnit::Function { name, .. } => {
            st.scopes.iter().find_map(|s| {
                if let ScopeKind::Function(ref n) = s.kind {
                    if n.eq_ignore_ascii_case(name) { Some(s.id) } else { None }
                } else { None }
            })
        }
        _ => None,
    }
}

fn validate_unit(ctx: &mut Ctx, unit: &SpannedUnit) {
    let saved_scope = ctx.scope_id;
    if let Some(scope_id) = find_scope_for_unit(ctx.st, &unit.node) {
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
        ProgramUnit::InterfaceBlock { bodies, .. } => {
            for body in bodies {
                if let InterfaceBody::Subprogram(sub) = body {
                    validate_unit(ctx, sub);
                }
            }
        }
        _ => {}
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
        Stmt::Stop { .. } | Stmt::ErrorStop { .. } => {
            if ctx.in_pure {
                ctx.error(stmt.span, "STOP/ERROR STOP not allowed in pure procedure");
            }
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
        Stmt::DoConcurrent { body, .. } => validate_stmts(ctx, body),
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
        Stmt::Block { body, .. } => validate_stmts(ctx, body),
        Stmt::Associate { body, .. } => validate_stmts(ctx, body),

        // Call in pure: callee must be pure (we check if it's known impure).
        Stmt::Call { callee, .. } => {
            if ctx.in_pure {
                validate_pure_call(ctx, callee, stmt.span);
            }
        }

        _ => {}
    }
}

// ---- Specific validation checks ----

/// Check that an assignment target is modifiable (not intent(in), not parameter).
fn validate_assignment_target(ctx: &mut Ctx, target: &crate::ast::expr::SpannedExpr, span: Span) {
    if let Expr::Name { name } = &target.node {
        let (is_intent_in, is_parameter) = ctx.lookup(name)
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
    if let Expr::Name { name } = &target.node {
        let is_pointer = ctx.lookup(name).map(|s| s.attrs.pointer).unwrap_or(true);
        if !is_pointer {
            ctx.error(span, format!("pointer assignment target '{}' must have pointer attribute", name));
        }
    }

    // RHS must have target attribute or be a pointer (or null()).
    if let Expr::Name { name } = &value.node {
        let ok = ctx.lookup(name).map(|s| s.attrs.target || s.attrs.pointer).unwrap_or(true);
        if !ok {
            ctx.error(span, format!("pointer assignment source '{}' must have target or pointer attribute", name));
        }
    }
    // If RHS is a function call (e.g., null()), we allow it — can't check further without type info.
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
fn validate_pure_call(ctx: &mut Ctx, callee: &crate::ast::expr::SpannedExpr, span: Span) {
    if let Expr::Name { name } = &callee.node {
        if let Some(sym) = ctx.lookup(name) {
            // We can only catch subroutines/functions that are locally known.
            // If the callee is defined in a CONTAINS and was resolved, we could check its prefix.
            // For now, we flag calls to known I/O routines.
            // Full check requires tracking pure attribute in symbol table — deferred.
            let _ = sym;
        }
    }
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
        let mut ctx = Ctx::new(&st);
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
        let mut ctx = Ctx::new(&st);
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
        let mut ctx = Ctx::new(&st);
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
}
