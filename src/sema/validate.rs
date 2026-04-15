//! Semantic validation — checks that go beyond type checking.
//!
//! Allocatable/pointer semantics, intent enforcement, pure/elemental
//! constraints, label validation, and standard conformance. Runs after
//! symbol resolution (resolve.rs) and type checking (types.rs).

use crate::ast::unit::*;
use crate::ast::stmt::*;
use crate::ast::expr::Expr;
use crate::ast::decl::{Decl, Attribute, TypeAttr, TypeSpec};
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
    pub fn parse_flag(s: &str) -> Option<Self> {
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
    /// Derived-type layouts — consulted when validating attribute-
    /// sensitive targets on a component access (`obj%field`), where
    /// the base variable's attributes aren't the right thing to check.
    type_layouts: Option<&'a crate::sema::type_layout::TypeLayoutRegistry>,
    warn_pedantic: bool,
    warn_deprecated: bool,
}

impl<'a> Ctx<'a> {
    fn new(
        st: &'a SymbolTable,
        std: Option<FortranStandard>,
        warn_pedantic: bool,
        warn_deprecated: bool,
    ) -> Self {
        Self {
            st,
            diags: Vec::new(),
            scope_id: 0,
            in_pure: false,
            in_elemental: false,
            std,
            labels_defined: Vec::new(),
            labels_referenced: Vec::new(),
            type_layouts: None,
            warn_pedantic,
            warn_deprecated,
        }
    }

    fn new_with_layouts(
        st: &'a SymbolTable,
        std: Option<FortranStandard>,
        type_layouts: &'a crate::sema::type_layout::TypeLayoutRegistry,
        warn_pedantic: bool,
        warn_deprecated: bool,
    ) -> Self {
        let mut ctx = Self::new(st, std, warn_pedantic, warn_deprecated);
        ctx.type_layouts = Some(type_layouts);
        ctx
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
    validate_file_with_warning_groups(units, st, std, false, false)
}

pub fn validate_file_with_warning_groups(
    units: &[SpannedUnit],
    st: &SymbolTable,
    std: Option<FortranStandard>,
    warn_pedantic: bool,
    warn_deprecated: bool,
) -> Vec<Diagnostic> {
    let mut ctx = Ctx::new(st, std, warn_pedantic, warn_deprecated);
    for unit in units {
        validate_unit(&mut ctx, unit);
    }
    ctx.diags
}

/// Validate with access to derived-type layouts, enabling per-field
/// attribute checks on ALLOCATE / pointer-assignment targets that
/// select a component (`obj%comp`).
pub fn validate_file_with_layouts(
    units: &[SpannedUnit],
    st: &SymbolTable,
    std: Option<FortranStandard>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> Vec<Diagnostic> {
    validate_file_with_layouts_and_warning_groups(
        units,
        st,
        std,
        type_layouts,
        false,
        false,
    )
}

pub fn validate_file_with_layouts_and_warning_groups(
    units: &[SpannedUnit],
    st: &SymbolTable,
    std: Option<FortranStandard>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    warn_pedantic: bool,
    warn_deprecated: bool,
) -> Vec<Diagnostic> {
    let mut ctx = Ctx::new_with_layouts(
        st,
        std,
        type_layouts,
        warn_pedantic,
        warn_deprecated,
    );
    for unit in units {
        validate_unit(&mut ctx, unit);
    }
    ctx.diags
}

fn warn_legacy_feature(ctx: &mut Ctx<'_>, span: Span, feature: &str) {
    if ctx.warn_pedantic || ctx.warn_deprecated {
        ctx.warning(span, format!("{} is an obsolescent feature", feature));
    }
}

/// Find the scope ID for a program unit, preferring children of `parent_scope`.
/// This resolves ambiguity when multiple scopes share a name (e.g., a module
/// subroutine and a CONTAINS subroutine with the same name).
fn find_scope_for_unit(
    st: &SymbolTable,
    unit: &ProgramUnit,
    parent_scope: ScopeId,
) -> Option<ScopeId> {
    #[allow(clippy::type_complexity)]
    let (kind_matcher, _name): (Box<dyn Fn(&ScopeKind) -> bool>, Option<String>) = match unit {
        ProgramUnit::Program { name, .. } => {
            let target = name.clone().unwrap_or_else(|| "<main>".into());
            (
                Box::new(move |k| matches!(k, ScopeKind::Program(ref n) if n == &target)),
                None,
            )
        }
        ProgramUnit::Module { name, .. } => {
            let n = name.clone();
            (
                Box::new(
                    move |k| matches!(k, ScopeKind::Module(ref m) if m.eq_ignore_ascii_case(&n)),
                ),
                Some(name.clone()),
            )
        }
        ProgramUnit::Subroutine { name, .. } => {
            let n = name.clone();
            (
                Box::new(
                    move |k| matches!(k, ScopeKind::Subroutine(ref m) if m.eq_ignore_ascii_case(&n)),
                ),
                Some(name.clone()),
            )
        }
        ProgramUnit::Function { name, .. } => {
            let n = name.clone();
            (
                Box::new(
                    move |k| matches!(k, ScopeKind::Function(ref m) if m.eq_ignore_ascii_case(&n)),
                ),
                Some(name.clone()),
            )
        }
        ProgramUnit::BlockData { name, .. } => {
            let target = name.clone().unwrap_or_else(|| "<block_data>".into());
            (
                Box::new(move |k| matches!(k, ScopeKind::Program(ref n) if n == &target)),
                None,
            )
        }
        _ => return None,
    };

    // Prefer a child of the current parent scope.
    let child = st
        .scopes
        .iter()
        .find(|s| s.parent == Some(parent_scope) && kind_matcher(&s.kind));
    if let Some(s) = child {
        return Some(s.id);
    }

    // Fall back to any matching scope.
    st.scopes
        .iter()
        .find(|s| kind_matcher(&s.kind))
        .map(|s| s.id)
}

fn validate_unit(ctx: &mut Ctx, unit: &SpannedUnit) {
    let saved_scope = ctx.scope_id;
    if let Some(scope_id) = find_scope_for_unit(ctx.st, &unit.node, ctx.scope_id) {
        ctx.scope_id = scope_id;
    }

    match &unit.node {
        ProgramUnit::Program {
            uses,
            implicit,
            decls,
            body,
            contains,
            ..
        } => {
            for use_stmt in uses {
                ctx.require_std(use_stmt.span, FortranStandard::F90, "USE statement");
            }
            for implicit_stmt in implicit {
                if matches!(implicit_stmt.node, Decl::ImplicitNone { .. }) {
                    ctx.require_std(
                        implicit_stmt.span,
                        FortranStandard::F90,
                        "IMPLICIT NONE",
                    );
                }
            }
            if !contains.is_empty() {
                ctx.require_std(unit.span, FortranStandard::F90, "CONTAINS/internal procedures");
            }
            validate_decls(ctx, decls);
            check_implicit_none(ctx, body, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            for sub in contains {
                validate_unit(ctx, sub);
            }
        }
        ProgramUnit::Module {
            uses,
            implicit,
            decls, contains, ..
        } => {
            ctx.require_std(unit.span, FortranStandard::F90, "MODULE");
            for use_stmt in uses {
                ctx.require_std(use_stmt.span, FortranStandard::F90, "USE statement");
            }
            for implicit_stmt in implicit {
                if matches!(implicit_stmt.node, Decl::ImplicitNone { .. }) {
                    ctx.require_std(
                        implicit_stmt.span,
                        FortranStandard::F90,
                        "IMPLICIT NONE",
                    );
                }
            }
            validate_decls(ctx, decls);
            for sub in contains {
                validate_unit(ctx, sub);
            }
        }
        ProgramUnit::Subroutine {
            prefix,
            uses,
            implicit,
            decls,
            body,
            contains,
            args,
            ..
        } => {
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

            for p in prefix {
                match p {
                    Prefix::Pure | Prefix::Elemental => {
                        ctx.require_std(unit.span, FortranStandard::F95, "PURE/ELEMENTAL");
                    }
                    Prefix::Impure => {
                        ctx.require_std(unit.span, FortranStandard::F2008, "IMPURE");
                    }
                    Prefix::Recursive => {
                        ctx.require_std(unit.span, FortranStandard::F90, "RECURSIVE");
                    }
                    _ => {}
                }
            }
            for use_stmt in uses {
                ctx.require_std(use_stmt.span, FortranStandard::F90, "USE statement");
            }
            for implicit_stmt in implicit {
                if matches!(implicit_stmt.node, Decl::ImplicitNone { .. }) {
                    ctx.require_std(
                        implicit_stmt.span,
                        FortranStandard::F90,
                        "IMPLICIT NONE",
                    );
                }
            }
            if !contains.is_empty() {
                ctx.require_std(unit.span, FortranStandard::F90, "CONTAINS/internal procedures");
            }
            validate_decls(ctx, decls);
            check_implicit_none(ctx, body, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            for sub in contains {
                validate_unit(ctx, sub);
            }
            ctx.in_pure = saved_pure;
            ctx.in_elemental = saved_elemental;
        }
        ProgramUnit::Function {
            prefix,
            uses,
            implicit,
            decls,
            body,
            contains,
            args,
            ..
        } => {
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

            for p in prefix {
                match p {
                    Prefix::Pure | Prefix::Elemental => {
                        ctx.require_std(unit.span, FortranStandard::F95, "PURE/ELEMENTAL");
                    }
                    Prefix::Impure => {
                        ctx.require_std(unit.span, FortranStandard::F2008, "IMPURE");
                    }
                    Prefix::Recursive => {
                        ctx.require_std(unit.span, FortranStandard::F90, "RECURSIVE");
                    }
                    _ => {}
                }
            }
            for use_stmt in uses {
                ctx.require_std(use_stmt.span, FortranStandard::F90, "USE statement");
            }
            for implicit_stmt in implicit {
                if matches!(implicit_stmt.node, Decl::ImplicitNone { .. }) {
                    ctx.require_std(
                        implicit_stmt.span,
                        FortranStandard::F90,
                        "IMPLICIT NONE",
                    );
                }
            }
            if !contains.is_empty() {
                ctx.require_std(unit.span, FortranStandard::F90, "CONTAINS/internal procedures");
            }
            validate_decls(ctx, decls);
            check_implicit_none(ctx, body, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            for sub in contains {
                validate_unit(ctx, sub);
            }
            ctx.in_pure = saved_pure;
            ctx.in_elemental = saved_elemental;
        }
        ProgramUnit::Submodule {
            uses,
            decls, contains, ..
        } => {
            ctx.require_std(unit.span, FortranStandard::F2008, "SUBMODULE");
            for use_stmt in uses {
                ctx.require_std(use_stmt.span, FortranStandard::F90, "USE statement");
            }
            validate_decls(ctx, decls);
            for sub in contains {
                validate_unit(ctx, sub);
            }
        }
        ProgramUnit::BlockData { decls, .. } => {
            warn_legacy_feature(ctx, unit.span, "BLOCK DATA");
            validate_decls(ctx, decls);
        }
        ProgramUnit::InterfaceBlock {
            name,
            is_abstract,
            bodies,
        } => {
            ctx.require_std(unit.span, FortranStandard::F90, "INTERFACE block");
            // Validate defined operator interfaces.
            if let Some(ref iface_name) = name {
                if is_operator_interface(iface_name) {
                    validate_operator_interface(ctx, iface_name, bodies, unit.span);
                }
            }
            // Abstract interfaces cannot have MODULE PROCEDURE.
            if *is_abstract {
                ctx.require_std(unit.span, FortranStandard::F2003, "ABSTRACT interface");
                for body in bodies {
                    if let InterfaceBody::ModuleProcedure(names) = body {
                        if !names.is_empty() {
                            ctx.error(
                                unit.span,
                                "abstract interface cannot contain MODULE PROCEDURE statements",
                            );
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
        if let Decl::TypeDecl {
            attrs,
            entities,
            type_spec,
            ..
        } = &decl.node
        {
            let has_alloc = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
            let has_pointer = attrs.iter().any(|a| matches!(a, Attribute::Pointer));
            let is_scalar_decl = entities.iter().all(|entity| entity.array_spec.is_none());

            // Deferred-length character must be allocatable or pointer.
            if let crate::ast::decl::TypeSpec::Character(Some(sel)) = type_spec {
                if let Some(crate::ast::decl::LenSpec::Colon) = &sel.len {
                    ctx.require_std(
                        decl.span,
                        FortranStandard::F2003,
                        "deferred-length character",
                    );
                    if !has_alloc && !has_pointer {
                        ctx.error(decl.span, "deferred-length character (len=:) requires allocatable or pointer attribute");
                    }
                }
            }

            match type_spec {
                TypeSpec::Class(_) => {
                    ctx.require_std(decl.span, FortranStandard::F2003, "CLASS declaration");
                }
                TypeSpec::ClassStar | TypeSpec::TypeStar => {
                    ctx.require_std(
                        decl.span,
                        FortranStandard::F2018,
                        "CLASS(*)/TYPE(*) declaration",
                    );
                }
                _ => {}
            }

            if has_alloc && is_scalar_decl {
                ctx.require_std(
                    decl.span,
                    FortranStandard::F2003,
                    "allocatable scalar variables",
                );
            }

            // Allocatable + pointer is forbidden.
            if has_alloc && has_pointer {
                ctx.error(
                    decl.span,
                    "a variable cannot be both allocatable and pointer",
                );
            }

            // Parameter with allocatable/pointer is forbidden.
            let has_param = attrs.iter().any(|a| matches!(a, Attribute::Parameter));
            if has_param && has_alloc {
                ctx.error(
                    decl.span,
                    "a named constant (parameter) cannot be allocatable",
                );
            }
            if has_param && has_pointer {
                ctx.error(
                    decl.span,
                    "a named constant (parameter) cannot be a pointer",
                );
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

        if matches!(decl.node, Decl::ImplicitNone { .. }) {
            ctx.require_std(decl.span, FortranStandard::F90, "IMPLICIT NONE");
        }

        if matches!(decl.node, Decl::UseStmt { .. }) {
            ctx.require_std(decl.span, FortranStandard::F90, "USE statement");
        }

        if matches!(decl.node, Decl::CommonBlock { .. }) {
            warn_legacy_feature(ctx, decl.span, "COMMON block");
        }

        if matches!(decl.node, Decl::EquivalenceStmt { .. }) {
            warn_legacy_feature(ctx, decl.span, "EQUIVALENCE");
        }

        // Derived type definition validation.
        if let Decl::DerivedTypeDef {
            name,
            attrs: type_attrs,
            type_bound_procs,
            components,
            ..
        } = &decl.node
        {
            ctx.require_std(decl.span, FortranStandard::F90, "derived types");
            if type_attrs
                .iter()
                .any(|attr| matches!(attr, TypeAttr::Abstract))
            {
                ctx.require_std(decl.span, FortranStandard::F2003, "ABSTRACT type");
            }
            validate_derived_type(
                ctx,
                name,
                type_attrs,
                type_bound_procs,
                components,
                decl.span,
            );
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
        Stmt::Assignment { target, value } => {
            validate_assignment_target(ctx, target, stmt.span);
            reject_pure_nonlocal_definition(ctx, target, stmt.span, "assignment");
            if ctx.in_pure { check_pure_expr_calls(ctx, value); }
        }
        Stmt::PointerAssignment { target, value, .. } => {
            validate_pointer_assignment(ctx, target, value, stmt.span);
            reject_pure_nonlocal_definition(ctx, target, stmt.span, "pointer assignment");
        }

        // ---- Allocate / Deallocate ----
        Stmt::Allocate { items, opts } => {
            if opts.iter().any(|opt| {
                opt.keyword
                    .as_deref()
                    .is_some_and(|kw| kw.eq_ignore_ascii_case("source"))
            }) {
                ctx.require_std(stmt.span, FortranStandard::F2003, "ALLOCATE with SOURCE=");
            }
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
            warn_legacy_feature(ctx, stmt.span, "computed GOTO");
            for label in labels {
                ctx.labels_referenced.push((*label, stmt.span));
            }
        }
        Stmt::ArithmeticIf { neg, zero, pos, .. } => {
            warn_legacy_feature(ctx, stmt.span, "arithmetic IF");
            ctx.labels_referenced.push((*neg, stmt.span));
            ctx.labels_referenced.push((*zero, stmt.span));
            ctx.labels_referenced.push((*pos, stmt.span));
        }
        Stmt::Continue { label: Some(lbl) } => {
            register_label(ctx, *lbl, stmt.span);
        }
        Stmt::Labeled { label, stmt: inner } => {
            register_label(ctx, *label, stmt.span);
            validate_stmt(ctx, inner);
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
        Stmt::WhereConstruct {
            body, elsewhere, ..
        } => {
            validate_stmts(ctx, body);
            for (_, ebody) in elsewhere {
                validate_stmts(ctx, ebody);
            }
        }
        Stmt::WhereStmt { stmt: inner, .. } => validate_stmt(ctx, inner),
        Stmt::ForallConstruct { body, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F95, "FORALL construct");
            validate_stmts(ctx, body);
        }
        Stmt::ForallStmt { stmt: inner, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F95, "FORALL statement");
            validate_stmt(ctx, inner);
        }
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
            if let Expr::Name { name } = &callee.node {
                if name.eq_ignore_ascii_case("move_alloc") {
                    ctx.require_std(stmt.span, FortranStandard::F2003, "MOVE_ALLOC");
                }
            }
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
    // Component-access target (`p%ptr_field => x`): check the leaf
    // component's attributes through the type-layout registry.  If
    // layouts aren't available (older callers) or the chain can't be
    // resolved, skip the check rather than flag the base variable.
    if expr_selects_component(target) {
        if let Some(leaf) = leaf_field_layout(ctx, target) {
            if !leaf.field.pointer {
                ctx.error(span, format!(
                    "pointer assignment target component '{}' must have pointer attribute",
                    leaf.field.name
                ));
            }
        }
    } else if let Some(name) = extract_base_name(target) {
        let is_pointer = ctx.lookup(&name).map(|s| s.attrs.pointer).unwrap_or(true);
        if !is_pointer {
            ctx.error(span, format!("pointer assignment target '{}' must have pointer attribute", name));
        }
    }

    // RHS must have target attribute or be a pointer (or null()/function call).
    if expr_selects_component(value) {
        // Look up the leaf component's attributes.  F2018 §8.5.14
        // says a subobject of a TARGET base (or an allocated
        // ALLOCATABLE) is itself a valid target, so accept when any
        // ancestor on the path carries one of those attributes.
        if let Some(leaf) = leaf_field_layout(ctx, value) {
            let ok = leaf.field.pointer
                || leaf.field.target
                || leaf.ancestor_is_target
                || leaf.ancestor_is_allocatable;
            if !ok {
                ctx.error(span, format!(
                    "pointer assignment source component '{}' must have target or pointer attribute",
                    leaf.field.name
                ));
            }
        }
        return;
    }
    if let Some(name) = extract_base_name(value) {
        // Skip if the value is a function call — could be null() or pointer-valued function.
        if matches!(value.node, Expr::FunctionCall { .. }) {
            return;
        }
        // Dummy procedure arguments are valid RHS targets per F2003
        // (their addresses are implicitly available). The generic
        // target/pointer check below doesn't see the "dummy
        // procedure" attribute directly; accept any Function/
        // Subroutine symbol and any variable declared via
        // `procedure(iface)` (parsed with Attribute::External).
        if let Some(sym) = ctx.lookup(&name) {
            use crate::sema::symtab::SymbolKind;
            if matches!(sym.kind, SymbolKind::Function | SymbolKind::Subroutine) {
                return;
            }
            if sym.attrs.external {
                return;
            }
        }
        let ok = ctx.lookup(&name).map(|s| s.attrs.target || s.attrs.pointer).unwrap_or(true);
        if !ok {
            ctx.error(span, format!("pointer assignment source '{}' must have target or pointer attribute", name));
        }
    }
}

/// Validate that an ALLOCATE/DEALLOCATE item is allocatable or pointer.
///
/// For a component access like `pools(i)%tokens(n)`, the target is
/// the `tokens` field — not the `pools` base.  Resolve the leaf
/// component through the type-layout registry and check its own
/// attributes.  Bare-name targets still get the symbol attribute
/// check.  If the chain can't be resolved (registry missing, cross-
/// TU stale .amod, etc.) we skip rather than produce a misleading
/// error.
fn validate_allocatable_item(ctx: &mut Ctx, item: &crate::ast::expr::SpannedExpr, stmt_name: &str) {
    if expr_selects_component(item) {
        if let Some(leaf) = leaf_field_layout(ctx, item) {
            if !leaf.field.allocatable && !leaf.field.pointer {
                ctx.error(item.span, format!(
                    "only allocatable or pointer components can appear in {}, but '{}' is neither",
                    stmt_name.to_uppercase(), leaf.field.name
                ));
            }
        }
        return;
    }
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

/// Does this expression select into a derived-type component
/// anywhere in its path? e.g. `pools(i)%tokens(n)` → true,
/// `pools(i)` → false, `pools` → false.
fn expr_selects_component(expr: &crate::ast::expr::SpannedExpr) -> bool {
    match &expr.node {
        Expr::ComponentAccess { .. } => true,
        Expr::FunctionCall { callee, .. } => expr_selects_component(callee),
        _ => false,
    }
}

/// Resolved metadata for the leaf of a component access.
struct LeafComponent<'a> {
    field: &'a crate::sema::type_layout::FieldLayout,
    /// Any ancestor on the path (including the base variable or any
    /// intermediate component) has the TARGET attribute.  F2018
    /// §8.5.14: a subobject of a TARGET is itself a valid target.
    ancestor_is_target: bool,
    /// Any ancestor is ALLOCATABLE — per §8.5.14, an allocated
    /// subobject of an allocatable is also a valid target.
    ancestor_is_allocatable: bool,
}

/// Walk an expression down to its leaf component access and return
/// that component's FieldLayout (with attribute metadata).  Returns
/// `None` if the expression has no component access, or if the
/// chain's derived-type path can't be resolved through the symbol
/// table + layout registry (for example, a field whose type is a
/// derived type that wasn't in the registry — uncommon but possible
/// when a cross-TU .amod is stale).
fn leaf_field_layout<'a>(
    ctx: &'a Ctx,
    expr: &crate::ast::expr::SpannedExpr,
) -> Option<LeafComponent<'a>> {
    let layouts = ctx.type_layouts?;
    // Collect the component chain from outermost to innermost.
    let mut chain: Vec<&str> = Vec::new();
    let mut cur = expr;
    let base_name = loop {
        match &cur.node {
            Expr::ComponentAccess { base, component } => {
                chain.push(component.as_str());
                cur = base;
            }
            Expr::FunctionCall { callee, .. } => {
                cur = callee;
            }
            Expr::Name { name } => break name.as_str(),
            _ => return None,
        }
    };
    chain.reverse();
    if chain.is_empty() { return None; }
    // Resolve the base variable's derived type via the symbol table.
    let sym = ctx.lookup(base_name)?;
    let base_type = match sym.type_info.as_ref()? {
        crate::sema::symtab::TypeInfo::Derived(name) => name.clone(),
        _ => return None,
    };
    // Seed ancestor flags from the base variable's own attributes.
    let mut ancestor_is_target = sym.attrs.target;
    let mut ancestor_is_allocatable = sym.attrs.allocatable;
    let mut current_type = base_type;
    let mut leaf: Option<&crate::sema::type_layout::FieldLayout> = None;
    for (i, comp) in chain.iter().enumerate() {
        let layout = layouts.get(&current_type)?;
        let field = layout.field(comp)?;
        // On non-terminal components, accumulate TARGET / ALLOCATABLE
        // so the leaf check can honour inherited target-ness.
        let is_terminal = i + 1 == chain.len();
        if !is_terminal {
            if field.target { ancestor_is_target = true; }
            if field.allocatable { ancestor_is_allocatable = true; }
        }
        leaf = Some(field);
        match &field.type_info {
            crate::sema::symtab::TypeInfo::Derived(name) => {
                current_type = name.clone();
            }
            _ => {
                // Scalar / intrinsic-typed leaf — no further resolution.
            }
        }
    }
    leaf.map(|field| LeafComponent {
        field,
        ancestor_is_target,
        ancestor_is_allocatable,
    })
}

/// Check if a call in a pure procedure is to a known impure procedure.
/// Symbol-level pure tracking isn't yet wired into the symbol table,
/// so this is conservative: we warn if the callee resolves to an
/// external procedure (whose body we cannot inspect).  I/O, STOP,
/// and SAVE violations are caught statement-level in validate_stmt.
/// Walk an expression tree and check any function calls against the
/// pure-call constraint.  Catches `r = impure_fn()` which is an
/// expression-level call, not a `Stmt::Call`.
fn check_pure_expr_calls(ctx: &mut Ctx, expr: &crate::ast::expr::SpannedExpr) {
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

fn validate_pure_call(ctx: &mut Ctx, callee: &crate::ast::expr::SpannedExpr, span: Span) {
    // F2018 15.7: a PURE procedure may only call PURE, ELEMENTAL,
    // or intrinsic procedures.  If the callee resolves to a known
    // symbol that is NOT marked pure/elemental/intrinsic, reject.
    // Unknown callees (external without an interface) are left
    // alone — the programmer's responsibility per F2018 §15.4.
    let Some(name) = extract_base_name(callee) else { return; };
    let Some(sym) = ctx.lookup(&name) else { return; };
    match sym.kind {
        SymbolKind::Function | SymbolKind::Subroutine => {
            if !sym.attrs.pure && !sym.attrs.elemental && !sym.attrs.intrinsic {
                ctx.error(
                    span,
                    format!(
                        "call to '{}' inside a pure procedure: callee is not pure, elemental, or intrinsic (F2018 15.7)",
                        sym.name
                    ),
                );
            }
        }
        SymbolKind::IntrinsicProc => {} // always OK
        _ => {} // external / unknown — can't check
    }
}

/// True if `sym` is declared outside the procedure rooted at
/// `procedure_scope` — i.e. it comes from host association, USE
/// association, or a COMMON block in an enclosing unit.  This is
/// the F2018 15.7 "accessed by host or use association, or in
/// common" predicate that makes a variable off-limits for
/// definition inside a PURE procedure body.
fn symbol_is_non_local_to_procedure(
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
fn reject_pure_nonlocal_definition(
    ctx: &mut Ctx,
    target: &crate::ast::expr::SpannedExpr,
    span: Span,
    stmt_label: &str,
) {
    if !ctx.in_pure {
        return;
    }
    let Some(name) = extract_base_name(target) else { return; };
    let Some(sym) = ctx.lookup(&name) else { return; };
    // Only variables and COMMON blocks can be "defined"; function
    // names get definition semantics too but those are the pure
    // function's own result variable (always local).
    if !matches!(sym.kind, SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::CommonBlock) {
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

// ---- IMPLICIT NONE enforcement ----

/// Check that all variable references in a statement list are declared
/// when IMPLICIT NONE is active in the current scope.
fn check_implicit_none(ctx: &mut Ctx, stmts: &[SpannedStmt], decls: &[crate::ast::decl::SpannedDecl]) {
    if !ctx.st.is_implicit_none(ctx.scope_id) { return; }

    // Collect declared names in this scope (from declarations).
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl { entities, .. } => {
                for e in entities {
                    declared.insert(e.name.to_lowercase());
                }
            }
            // COMMON block variables are also declared.
            Decl::CommonBlock { vars, .. } => {
                for v in vars {
                    declared.insert(v.to_lowercase());
                }
            }
            _ => {}
        }
    }
    // Also scan for INTERFACE blocks — function/subroutine names
    // declared in interfaces are valid in the current scope.
    // The interface bodies are stored as program units in the
    // ifaces/contains lists, not in decls. But the symbol table
    // should have them via resolve. We also check decls for
    // EXTERNAL statements.
    for decl in decls {
        if let Decl::TypeDecl { attrs, entities, .. } = &decl.node {
            if attrs.iter().any(|a| matches!(a, Attribute::External)) {
                for e in entities {
                    declared.insert(e.name.to_lowercase());
                }
            }
        }
    }

    let mut undeclared = Vec::new();
    let outer_implicit_letters: std::collections::HashSet<char> =
        std::collections::HashSet::new();
    for stmt in stmts {
        walk_stmt_for_undeclared(
            ctx.st,
            ctx.scope_id,
            stmt,
            &declared,
            &outer_implicit_letters,
            &mut undeclared,
        );
    }

    // Deduplicate by name (only report each undeclared name once).
    let mut reported: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, span) in &undeclared {
        let key = name.to_lowercase();
        if reported.insert(key) {
            ctx.error(*span, format!(
                "variable '{}' used but not declared (IMPLICIT NONE is active)", name
            ));
        }
    }
}

fn walk_stmt_for_undeclared(
    st: &SymbolTable,
    scope_id: ScopeId,
    stmt: &SpannedStmt,
    declared: &std::collections::HashSet<String>,
    implicit_letters: &std::collections::HashSet<char>,
    undeclared: &mut Vec<(String, Span)>,
) {
    macro_rules! chk {
        ($e:expr) => { check_expr_names(st, scope_id, $e, declared, implicit_letters, undeclared) };
    }
    macro_rules! recurse {
        ($s:expr) => { walk_stmt_for_undeclared(st, scope_id, $s, declared, implicit_letters, undeclared) };
    }
    match &stmt.node {
        Stmt::Assignment { target, value } => {
            chk!(target); chk!(value);
        }
        Stmt::PointerAssignment { target, value, .. } => {
            chk!(target); chk!(value);
        }
        Stmt::Print { items, .. } => {
            for item in items { chk!(item); }
        }
        Stmt::Write { items, controls, .. } => {
            for item in items { chk!(item); }
            for ctrl in controls { chk!(&ctrl.value); }
        }
        Stmt::Read { items, controls, .. } => {
            for item in items { chk!(item); }
            for ctrl in controls { chk!(&ctrl.value); }
        }
        Stmt::IfConstruct { condition, then_body, else_ifs, else_body, .. } => {
            chk!(condition);
            for s in then_body { recurse!(s); }
            for (cond, body) in else_ifs {
                chk!(cond);
                for s in body { recurse!(s); }
            }
            if let Some(body) = else_body {
                for s in body { recurse!(s); }
            }
        }
        Stmt::IfStmt { condition, action } => {
            chk!(condition); recurse!(action);
        }
        Stmt::DoLoop { body, .. } | Stmt::DoWhile { body, .. } |
        Stmt::DoConcurrent { body, .. } => {
            for s in body { recurse!(s); }
        }
        Stmt::Block { implicit, decls, body, .. } => {
            // F2018 §11.1.4: a BLOCK construct establishes its own
            // scope with an independent implicit-typing environment.
            // Layer the block's declared names AND any IMPLICIT
            // statements over the inherited rules; the local set
            // does not leak back out.
            let mut block_declared = declared.clone();
            for d in decls {
                if let crate::ast::decl::Decl::TypeDecl { entities, .. } = &d.node {
                    for e in entities {
                        block_declared.insert(e.name.to_lowercase());
                    }
                }
            }
            let mut block_implicit = implicit_letters.clone();
            let mut block_implicit_none = false;
            for d in implicit {
                match &d.node {
                    crate::ast::decl::Decl::ImplicitNone { .. } => {
                        block_implicit_none = true;
                    }
                    crate::ast::decl::Decl::ImplicitStmt { specs } => {
                        for spec in specs {
                            for &(start, end) in &spec.ranges {
                                for letter_byte in start as u8..=end as u8 {
                                    let letter = (letter_byte as char).to_ascii_lowercase();
                                    block_implicit.insert(letter);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // An IMPLICIT NONE inside the block clears the inherited
            // letter set rather than augmenting it.  Subsequent
            // IMPLICIT statements in the same block (rare but legal)
            // re-establish a covering range from scratch.
            if block_implicit_none {
                block_implicit.clear();
                for d in implicit {
                    if let crate::ast::decl::Decl::ImplicitStmt { specs } = &d.node {
                        for spec in specs {
                            for &(start, end) in &spec.ranges {
                                for letter_byte in start as u8..=end as u8 {
                                    let letter = (letter_byte as char).to_ascii_lowercase();
                                    block_implicit.insert(letter);
                                }
                            }
                        }
                    }
                }
            }
            for s in body {
                walk_stmt_for_undeclared(
                    st,
                    scope_id,
                    s,
                    &block_declared,
                    &block_implicit,
                    undeclared,
                );
            }
        }
        Stmt::SelectCase { selector, cases, .. } => {
            chk!(selector);
            for case in cases {
                for s in &case.body { recurse!(s); }
            }
        }
        Stmt::Call { args, .. } => {
            for arg in args {
                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                    chk!(e);
                }
            }
        }
        Stmt::Labeled { stmt: inner, .. } => { recurse!(inner); }
        Stmt::WhereConstruct { mask, body, elsewhere, .. } => {
            chk!(mask);
            for s in body { recurse!(s); }
            for (m, b) in elsewhere {
                if let Some(m) = m { chk!(m); }
                for s in b { recurse!(s); }
            }
        }
        _ => {}
    }
}

/// Walk an expression and collect undeclared Name references.
fn check_expr_names(
    st: &SymbolTable,
    scope_id: ScopeId,
    expr: &crate::ast::expr::SpannedExpr,
    declared: &std::collections::HashSet<String>,
    implicit_letters: &std::collections::HashSet<char>,
    undeclared: &mut Vec<(String, Span)>,
) {
    match &expr.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            // Skip format specifier * (appears in WRITE(*, *) / READ(*, *)).
            if key == "*" { return; }
            if declared.contains(&key) { return; }
            if st.lookup_in(scope_id, &key).is_some() { return; }
            if is_intrinsic_name(&key) { return; }
            // F2018 §11.1.4: a BLOCK-scoped IMPLICIT statement gives
            // names whose first letter is in the covered range an
            // implicit type, even if the enclosing scope is
            // IMPLICIT NONE.
            if let Some(first) = key.chars().next() {
                if implicit_letters.contains(&first.to_ascii_lowercase()) {
                    return;
                }
            }
            undeclared.push((name.clone(), expr.span));
        }
        Expr::BinaryOp { left, right, .. } => {
            check_expr_names(st, scope_id, left, declared, implicit_letters, undeclared);
            check_expr_names(st, scope_id, right, declared, implicit_letters, undeclared);
        }
        Expr::UnaryOp { operand, .. } => {
            check_expr_names(st, scope_id, operand, declared, implicit_letters, undeclared);
        }
        Expr::FunctionCall { callee, args } => {
            // Under IMPLICIT NONE the callee name must resolve to a
            // declared identifier: a host/module procedure visible
            // via `lookup_in`, an EXTERNAL dummy (already in
            // `declared`), or an intrinsic. The `declared` set and
            // lookup path in the bare-Name arm handle all three;
            // reuse it so `foo(3)` with no declaration of `foo` is
            // rejected at compile time instead of falling through to
            // a link error.
            check_expr_names(st, scope_id, callee, declared, implicit_letters, undeclared);
            for arg in args {
                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                    check_expr_names(st, scope_id, e, declared, implicit_letters, undeclared);
                }
            }
        }
        Expr::ComponentAccess { base, .. } => {
            check_expr_names(st, scope_id, base, declared, implicit_letters, undeclared);
        }
        Expr::ParenExpr { inner } => {
            check_expr_names(st, scope_id, inner, declared, implicit_letters, undeclared);
        }
        _ => {}
    }
}

pub fn is_intrinsic_name(name: &str) -> bool {
    matches!(name,
        "abs" | "iabs" | "dabs" | "cabs" | "acos" | "asin" | "atan" | "atan2" |
        "cos" | "sin" | "tan" | "exp" | "log" | "log10" | "sqrt" | "dsqrt" |
        "mod" | "modulo" | "max" | "min" | "sign" | "dim" |
        "int" | "nint" | "real" | "dble" | "cmplx" | "conjg" |
        "aimag" | "dimag" | "char" | "ichar" | "achar" | "iachar" |
        "len" | "len_trim" | "trim" | "adjustl" | "adjustr" |
        "index" | "scan" | "verify" | "repeat" | "lge" | "lgt" | "lle" | "llt" |
        "kind" | "selected_int_kind" | "selected_real_kind" |
        "size" | "shape" | "lbound" | "ubound" | "allocated" | "associated" |
        "present" | "merge" | "pack" | "unpack" | "spread" | "reshape" |
        "sum" | "product" | "maxval" | "minval" | "count" | "any" | "all" |
        "matmul" | "dot_product" | "transpose" |
        "huge" | "tiny" | "epsilon" | "precision" | "range" | "radix" |
        "maxexponent" | "minexponent" | "digits" | "bit_size" |
        "floor" | "ceiling" | "fraction" | "exponent" | "scale" |
        "ibset" | "ibclr" | "ibits" | "btest" | "iand" | "ior" | "ieor" | "not" |
        "ishft" | "ishftc" | "mvbits" | "transfer" |
        "new_line" | "null" | "move_alloc" |
        "system_clock" | "date_and_time" | "cpu_time" | "random_number" | "random_seed" |
        "command_argument_count" | "get_command_argument" | "get_environment_variable" |
        "execute_command_line" | "compiler_version" | "compiler_options" |
        "c_loc" | "c_funloc" | "c_f_pointer" | "c_associated" | "c_sizeof" |
        "ieee_is_nan" | "ieee_is_finite" | "ieee_value" |
        "ieee_support_datatype" | "ieee_support_denormal" |
        "ieee_selected_real_kind" |
        // Statement-like names that can appear in expression context
        "float" | "dfloat" | "sngl" | "idint" | "ifix" | "idnint" |
        "dprod" | "dmax1" | "dmin1" | "max0" | "min0" | "max1" | "min1" |
        "amax0" | "amin0" | "amax1" | "amin1"
    )
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
        let rr = resolve::resolve_file(&units, &[]).unwrap(); let st = rr.st;
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
        let rr = resolve::resolve_file(&units, &[]).unwrap(); let st = rr.st;
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

    #[test]
    fn pure_write_to_module_variable_errors() {
        let errs = errors_from("\
module m
  integer :: counter = 0
contains
  pure integer function writes_counter() result(r)
    counter = 99
    r = counter
  end function
end module
");
        assert!(
            errs.iter().any(|e| e.contains("counter") && e.contains("pure") && e.contains("host or use association")),
            "expected pure+module-write error, got {:?}",
            errs,
        );
    }

    #[test]
    fn pure_read_of_module_variable_ok() {
        // F2018 15.7 permits a pure procedure to *reference* a
        // variable accessed by use association; only definition
        // is forbidden.  reads_counter is a legal pure function.
        let errs = errors_from("\
module m
  integer :: counter = 0
contains
  pure integer function reads_counter() result(r)
    r = counter
  end function
end module
");
        assert!(errs.is_empty(), "pure read of module variable should be legal, got {:?}", errs);
    }

    #[test]
    fn pure_write_to_host_variable_errors() {
        let errs = errors_from("\
program p
  integer :: host_var
  host_var = 0
  call helper()
contains
  pure subroutine helper()
    host_var = 42
  end subroutine
end program
");
        assert!(
            errs.iter().any(|e| e.contains("host_var") && e.contains("pure") && e.contains("host or use association")),
            "expected pure+host-write error, got {:?}",
            errs,
        );
    }

    #[test]
    fn pure_pointer_reassoc_of_module_pointer_errors() {
        let errs = errors_from("\
module m
  integer, pointer :: module_p
contains
  pure subroutine reassoc(t)
    integer, target, intent(in) :: t
    module_p => t
  end subroutine
end module
");
        assert!(
            errs.iter().any(|e| e.contains("module_p") && e.contains("pure") && e.contains("pointer assignment")),
            "expected pure+module-pointer error, got {:?}",
            errs,
        );
    }

    #[test]
    fn pure_local_pointer_reassoc_ok() {
        // Associating a LOCAL pointer with a module TARGET is
        // legal — `q => counter` does not modify `counter`.
        let errs = errors_from("\
module m
  integer, target :: counter = 0
contains
  pure integer function associates_counter() result(r)
    integer, pointer :: q
    q => counter
    r = 0
  end function
end module
");
        assert!(errs.is_empty(), "pure local pointer reassoc should be legal, got {:?}", errs);
    }

    #[test]
    fn pure_intent_out_dummy_ok() {
        let errs = errors_from("\
pure subroutine zero_it(x)
  integer, intent(out) :: x
  x = 0
end subroutine
");
        assert!(errs.is_empty(), "pure write to intent(out) dummy should be legal, got {:?}", errs);
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
        use crate::lexer::{Position, Span};
        let st = SymbolTable::new();
        let mut ctx = Ctx::new(&st, None, false, false);
        let span = Span {
            file_id: 0,
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        };

        // Reference label 999 but don't define it.
        ctx.labels_referenced.push((999, span));
        validate_label_consistency(&mut ctx, span);
        assert!(ctx.diags.iter().any(|d| d.msg.contains("label 999")));
    }

    #[test]
    fn goto_defined_label_no_error() {
        use crate::lexer::{Position, Span};
        let st = SymbolTable::new();
        let mut ctx = Ctx::new(&st, None, false, false);
        let span = Span {
            file_id: 0,
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        };

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
",
        );
        assert!(!errs.iter().any(|e| e.contains("requires")));
    }

    #[test]
    fn impure_requires_f2008() {
        let errs = errors_with_std(
            "\
impure subroutine s()
end subroutine
",
            FortranStandard::F95,
        );
        assert!(errs.iter().any(|e| e.contains("IMPURE") && e.contains("F2008")));
    }

    #[test]
    fn submodule_requires_f2008() {
        use crate::lexer::{Position, Span};

        let span = Span {
            file_id: 0,
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        };
        let unit = crate::ast::Spanned::new(
            ProgramUnit::Submodule {
                parent: "parent_mod".into(),
                ancestor: None,
                name: "child_mod".into(),
                uses: vec![],
                decls: vec![],
                contains: vec![],
            },
            span,
        );
        let diags = validate_file_with_std(&[unit], &SymbolTable::new(), Some(FortranStandard::F95));
        let errs: Vec<_> = diags
            .into_iter()
            .filter(|d| d.kind == DiagKind::Error)
            .map(|d| d.msg)
            .collect();
        assert!(errs.iter().any(|e| e.contains("SUBMODULE") && e.contains("F2008")));
    }

    #[test]
    fn abstract_type_requires_f2003() {
        let errs = errors_with_std(
            "\
module m
  type, abstract :: shape
  end type shape
end module
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("ABSTRACT type") && e.contains("F2003")));
    }

    #[test]
    fn class_star_requires_f2018() {
        let errs = errors_with_std(
            "\
subroutine s(x)
  class(*) :: x
end subroutine
",
            FortranStandard::F2008,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("CLASS(*)/TYPE(*) declaration") && e.contains("F2018")));
    }

    #[test]
    fn type_star_requires_f2018() {
        let errs = errors_with_std(
            "\
subroutine s(x)
  type(*) :: x
end subroutine
",
            FortranStandard::F2008,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("CLASS(*)/TYPE(*) declaration") && e.contains("F2018")));
    }

    #[test]
    fn deferred_length_character_requires_f2003() {
        let errs = errors_with_std(
            "\
program p
  character(len=:), allocatable :: s
end program
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("deferred-length character") && e.contains("F2003")));
    }

    #[test]
    fn allocatable_scalar_requires_f2003() {
        let errs = errors_with_std(
            "\
program p
  integer, allocatable :: x
end program
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("allocatable scalar variables") && e.contains("F2003")));
    }

    #[test]
    fn allocate_source_requires_f2003() {
        let errs = errors_with_std(
            "\
program p
  integer, allocatable :: x
  integer :: y
  allocate(x, source=y)
end program
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("ALLOCATE with SOURCE=") && e.contains("F2003")));
    }

    #[test]
    fn move_alloc_requires_f2003() {
        let errs = errors_with_std(
            "\
program p
  integer, allocatable :: x, y
  call move_alloc(x, y)
end program
",
            FortranStandard::F95,
        );
        assert!(errs.iter().any(|e| e.contains("MOVE_ALLOC") && e.contains("F2003")));
    }

    // ---- Elemental ----

    #[test]
    fn elemental_io_errors() {
        let errs = errors_from(
            "\
elemental subroutine foo(x)
  real, intent(in) :: x
  print *, x
end subroutine
",
        );
        // Elemental implies pure, so I/O is forbidden.
        assert!(errs.iter().any(|e| e.contains("I/O") && e.contains("pure")));
    }
}
