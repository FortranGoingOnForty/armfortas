//! Semantic validation — checks that go beyond type checking.
//!
//! Allocatable/pointer semantics, intent enforcement, pure/elemental
//! constraints, label validation, and standard conformance. Runs after
//! symbol resolution (resolve.rs) and type checking (types.rs).

use crate::sema::symtab::*;
use crate::sema::types::expr_type;

use super::allocatable::{
    allocate_item_needs_explicit_shape, expr_selects_component, leaf_field_layout,
    validate_allocatable_item,
};
use super::pointer::validate_pointer_assignment;
use super::pure_elemental::{
    check_pure_expr_calls, reject_pure_nonlocal_definition, validate_elemental_args,
    validate_pure_call,
};
use crate::ast::decl::{Attribute, Decl, SpannedDecl, TypeAttr, TypeSpec};
use crate::ast::expr::{Expr, SpannedExpr};
use crate::ast::stmt::*;
use crate::ast::unit::*;
use crate::lexer::Span;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

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
        write!(
            f,
            "{}:{}: {}: {}",
            self.span.start.line, self.span.start.col, label, self.msg
        )
    }
}

/// Validation context — accumulates diagnostics while walking the AST.
pub(super) struct Ctx<'a> {
    pub(super) st: &'a SymbolTable,
    pub(super) diags: Vec<Diagnostic>,
    /// Current scope ID for symbol lookups.
    pub(super) scope_id: ScopeId,
    /// Are we inside a pure procedure?
    pub(super) in_pure: bool,
    /// Are we inside an elemental procedure?
    pub(super) in_elemental: bool,
    /// Target standard for conformance checking (None = allow everything).
    pub(super) std: Option<FortranStandard>,
    /// Labels defined in the current scope.
    pub(super) labels_defined: Vec<u64>,
    /// Labels referenced (GOTO targets) in the current scope.
    pub(super) labels_referenced: Vec<(u64, Span)>,
    /// Derived-type layouts — consulted when validating attribute-
    /// sensitive targets on a component access (`obj%field`), where
    /// the base variable's attributes aren't the right thing to check.
    pub(super) type_layouts: Option<&'a crate::sema::type_layout::TypeLayoutRegistry>,
    /// Array names declared in each scope with allocatable/pointer storage.
    pub(super) allocatable_array_targets: HashSet<(ScopeId, String)>,
    pub(super) lookup_cache:
        RefCell<std::collections::HashMap<(ScopeId, String), Option<&'a Symbol>>>,
    /// Stack of associate-name frames. Each frame is the lowercase set of
    /// associate-names introduced by an enclosing ASSOCIATE construct. Names
    /// in any active frame shadow same-named USE-imported or host-scope
    /// symbols for purposes of validation (an associate-name aliases its
    /// selector, so parameter/intent attributes of a USE-imported symbol
    /// with the same name don't apply inside the body).
    pub(super) associate_frames: Vec<HashSet<String>>,
    /// Stack of BLOCK-local declarations. BLOCK constructs are
    /// scoping units, but the resolver does not currently materialize
    /// statement-level block scopes in the symbol table. Validation
    /// still needs those local declarations to shadow use/host names.
    block_decl_frames: Vec<HashMap<String, BlockBindingAttrs>>,
    pub(super) warn_pedantic: bool,
    pub(super) warn_deprecated: bool,
    /// Lowercase dummy-argument names of the unit currently being
    /// validated (empty for PROGRAM/MODULE scope). Used by the
    /// RANK(n) C-constraint check.
    pub(super) current_args: HashSet<String>,
    /// True while validating the actual arguments of a CALL statement —
    /// the only context where a conditional arm may be `.NIL.` and
    /// where conditional arguments select associations (F2023).
    pub(super) in_call_arg: bool,
    /// True while validating a conditional expression that is the direct
    /// RHS of an array assignment (or a directly-nested arm of one). Such
    /// conditionals lower via a per-arm branch (lower_array_conditional_assign),
    /// so array-valued arms are allowed here; everywhere else they are
    /// rejected because the merge has no descriptor lowering.
    pub(super) allow_array_cond_rhs: bool,
    /// True while validating a BIND(C) procedure (including interface
    /// bodies). BIND(C) `character(kind=c_char), value` dummies have a
    /// working byte-copy lowering; only the Fortran-internal character
    /// VALUE path lacks copy-in.
    pub(super) in_bind_c_unit: bool,
    /// Host scopes whose storage must not be captured by the procedure
    /// currently being validated because it is reachable from a local
    /// FINAL binding and may be invoked after those scopes return.
    finalizer_capture_host_scopes: HashSet<ScopeId>,
    /// Avoid repeating one unsupported-capture diagnostic for every use
    /// of the same host entity in a finalizer or one of its helpers.
    reported_finalizer_captures: HashSet<(ScopeId, ScopeId, String)>,
}

#[derive(Clone, Copy, Default)]
struct BlockBindingAttrs {
    intent_in: bool,
    parameter: bool,
    pointer: bool,
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
            allocatable_array_targets: HashSet::new(),
            lookup_cache: RefCell::new(std::collections::HashMap::new()),
            associate_frames: Vec::new(),
            block_decl_frames: Vec::new(),
            warn_pedantic,
            warn_deprecated,
            current_args: HashSet::new(),
            in_call_arg: false,
            allow_array_cond_rhs: false,
            in_bind_c_unit: false,
            finalizer_capture_host_scopes: HashSet::new(),
            reported_finalizer_captures: HashSet::new(),
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
                self.error(
                    span,
                    format!("{} requires --std={:?} or later", feature, min),
                );
            }
        }
    }

    /// True if `name` is currently bound by an enclosing ASSOCIATE
    /// construct. Associate-names alias their selector and shadow any
    /// USE-imported or host-scope symbol with the same name within the
    /// construct body.
    fn is_associate_name(&self, name: &str) -> bool {
        let key = name.to_lowercase();
        self.associate_frames
            .iter()
            .any(|frame| frame.contains(&key))
    }

    pub(super) fn is_block_local_name(&self, name: &str) -> bool {
        self.block_binding_attrs(name).is_some()
    }

    fn block_binding_attrs(&self, name: &str) -> Option<BlockBindingAttrs> {
        let key = name.to_lowercase();
        self.block_decl_frames
            .iter()
            .rev()
            .find_map(|frame| frame.get(&key).copied())
    }

    /// Look up a symbol in the current validation scope.
    pub(super) fn lookup(&self, name: &str) -> Option<&'a Symbol> {
        let key = (self.scope_id, name.to_lowercase());
        if let Some(cached) = self.lookup_cache.borrow().get(&key).copied() {
            return cached;
        }
        let resolved = self.st.lookup_in(self.scope_id, name);
        self.lookup_cache.borrow_mut().insert(key, resolved);
        resolved
    }

    pub(super) fn error(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic {
            span,
            kind: DiagKind::Error,
            msg: msg.into(),
        });
    }

    fn warning(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic {
            span,
            kind: DiagKind::Warning,
            msg: msg.into(),
        });
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
    let mut type_layouts = crate::sema::type_layout::TypeLayoutRegistry::new();
    crate::sema::resolve::compute_all_layouts(
        crate::target::TargetLayout::LP64,
        units,
        st,
        &mut type_layouts,
    );
    let mut ctx = Ctx::new_with_layouts(st, std, &type_layouts, warn_pedantic, warn_deprecated);
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
    validate_file_with_layouts_and_warning_groups(units, st, std, type_layouts, false, false)
}

pub fn validate_file_with_layouts_and_warning_groups(
    units: &[SpannedUnit],
    st: &SymbolTable,
    std: Option<FortranStandard>,
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    warn_pedantic: bool,
    warn_deprecated: bool,
) -> Vec<Diagnostic> {
    let mut ctx = Ctx::new_with_layouts(st, std, type_layouts, warn_pedantic, warn_deprecated);
    for unit in units {
        validate_unit(&mut ctx, unit);
    }
    ctx.diags
}

#[derive(Debug, Clone, Copy)]
struct ConstIntValue {
    value: i128,
    kind: u8,
}

#[derive(Debug, Clone, Copy)]
struct ConstIntError {
    span: Span,
    msg: &'static str,
}

fn default_int_kind(kind: Option<u8>) -> u8 {
    kind.unwrap_or_else(crate::driver::defaults::default_int_kind)
}

fn parse_int_literal_kind(ctx: &Ctx<'_>, kind: &Option<String>) -> u8 {
    if let Some(kind_name) = kind {
        if let Ok(n) = kind_name.parse::<u8>() {
            return n;
        }
        if let Some(sym) = ctx
            .lookup(kind_name)
            .or_else(|| ctx.st.find_symbol_any_scope(&kind_name.to_lowercase()))
        {
            if let Some(value) = sym.const_value.and_then(|v| u8::try_from(v).ok()) {
                return value;
            }
        }
    }
    crate::driver::defaults::default_int_kind()
}

fn int_kind_bounds(kind: u8) -> Option<(i128, i128)> {
    let bits = u32::from(kind).checked_mul(8)?;
    if bits == 0 {
        return None;
    }
    if bits >= 128 {
        return Some((i128::MIN, i128::MAX));
    }
    let shift = bits - 1;
    Some((-(1i128 << shift), (1i128 << shift) - 1))
}

/// `type(c_ptr)` / `type(c_funptr)` are interoperable opaque pointer
/// types from ISO_C_BINDING: ABI-wise a single 8-byte pointer (INTEGER
/// class — one GP register), not an aggregate. They lower as scalar
/// pointers, so a BIND(C) VALUE dummy or function result of these types
/// works and must be exempt from the derived-type by-value / return
/// rejections (audit C2). Every other derived type is a real aggregate.
fn is_c_interop_pointer_typespec(ts: &crate::ast::decl::TypeSpec) -> bool {
    matches!(
        ts,
        crate::ast::decl::TypeSpec::Type(name)
            if name.eq_ignore_ascii_case("c_ptr") || name.eq_ignore_ascii_case("c_funptr")
    )
}

/// Evaluate a REAL/COMPLEX kind selector to a concrete kind value when
/// it is an integer literal or a named integer constant in scope (a
/// PARAMETER such as `dp`, or an ISO_FORTRAN_ENV name like `real64`).
/// Returns None when the kind can't be determined statically — the
/// caller must not reject on an unknown kind. `real*16` (the old-style
/// `Star` selector) is evaluated the same way.
fn eval_real_complex_kind(
    ctx: &Ctx<'_>,
    sel: &Option<crate::ast::decl::KindSelector>,
) -> Option<u8> {
    use crate::ast::decl::KindSelector;
    use crate::ast::expr::Expr;
    let (KindSelector::Expr(e) | KindSelector::Star(e)) = sel.as_ref()?;
    match &e.node {
        Expr::IntegerLiteral { text, .. } => text.parse::<u8>().ok(),
        Expr::Name { name } => {
            let key = name.to_lowercase();
            ctx.st
                .lookup_in(ctx.scope_id, &key)
                .or_else(|| ctx.st.find_symbol_any_scope(&key))
                .and_then(|sym| sym.const_value)
                .and_then(|v| u8::try_from(v).ok())
        }
        _ => None,
    }
}

fn checked_int_value(value: i128, kind: u8, span: Span) -> Result<ConstIntValue, ConstIntError> {
    let Some((min, max)) = int_kind_bounds(kind) else {
        return Ok(ConstIntValue { value, kind });
    };
    if value < min || value > max {
        return Err(ConstIntError {
            span,
            msg: "compile-time integer overflow",
        });
    }
    Ok(ConstIntValue { value, kind })
}

fn const_int_kind_of_symbol(sym: &Symbol) -> Option<u8> {
    match &sym.type_info {
        Some(TypeInfo::Integer { kind }) => Some(default_int_kind(*kind)),
        _ => None,
    }
}

fn eval_const_int_expr_checked(
    ctx: &Ctx<'_>,
    expr: &crate::ast::expr::SpannedExpr,
) -> Result<Option<ConstIntValue>, ConstIntError> {
    match &expr.node {
        Expr::IntegerLiteral { text, kind } => {
            let clean = text.split('_').next().unwrap_or(text);
            let value = clean.parse::<i128>().map_err(|_| ConstIntError {
                span: expr.span,
                msg: "compile-time integer overflow",
            })?;
            checked_int_value(value, parse_int_literal_kind(ctx, kind), expr.span).map(Some)
        }
        Expr::Name { name } => {
            let Some(sym) = ctx.lookup(name) else {
                return Ok(None);
            };
            let Some(kind) = const_int_kind_of_symbol(sym) else {
                return Ok(None);
            };
            let Some(value) = sym.const_value.map(i128::from) else {
                return Ok(None);
            };
            checked_int_value(value, kind, expr.span).map(Some)
        }
        Expr::UnaryOp { op, operand } => {
            let Some(value) = eval_const_int_expr_checked(ctx, operand)? else {
                return Ok(None);
            };
            match op {
                crate::ast::expr::UnaryOp::Plus => {
                    checked_int_value(value.value, value.kind, expr.span).map(Some)
                }
                crate::ast::expr::UnaryOp::Minus => {
                    let negated = value.value.checked_neg().ok_or(ConstIntError {
                        span: expr.span,
                        msg: "compile-time integer overflow",
                    })?;
                    checked_int_value(negated, value.kind, expr.span).map(Some)
                }
                _ => Ok(None),
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let Some(left_value) = eval_const_int_expr_checked(ctx, left)? else {
                return Ok(None);
            };
            let Some(right_value) = eval_const_int_expr_checked(ctx, right)? else {
                return Ok(None);
            };
            let kind = left_value.kind.max(right_value.kind);
            let value = match op {
                crate::ast::expr::BinaryOp::Add => left_value
                    .value
                    .checked_add(right_value.value)
                    .ok_or(ConstIntError {
                        span: expr.span,
                        msg: "compile-time integer overflow",
                    })?,
                crate::ast::expr::BinaryOp::Sub => left_value
                    .value
                    .checked_sub(right_value.value)
                    .ok_or(ConstIntError {
                        span: expr.span,
                        msg: "compile-time integer overflow",
                    })?,
                crate::ast::expr::BinaryOp::Mul => left_value
                    .value
                    .checked_mul(right_value.value)
                    .ok_or(ConstIntError {
                        span: expr.span,
                        msg: "compile-time integer overflow",
                    })?,
                crate::ast::expr::BinaryOp::Div => {
                    if right_value.value == 0 {
                        return Err(ConstIntError {
                            span: right.span,
                            msg: "compile-time integer division by zero",
                        });
                    }
                    left_value
                        .value
                        .checked_div(right_value.value)
                        .ok_or(ConstIntError {
                            span: expr.span,
                            msg: "compile-time integer overflow",
                        })?
                }
                crate::ast::expr::BinaryOp::Pow => {
                    let Ok(exp) = u32::try_from(right_value.value) else {
                        return Ok(None);
                    };
                    left_value.value.checked_pow(exp).ok_or(ConstIntError {
                        span: expr.span,
                        msg: "compile-time integer overflow",
                    })?
                }
                _ => return Ok(None),
            };
            checked_int_value(value, kind, expr.span).map(Some)
        }
        Expr::ParenExpr { inner } => eval_const_int_expr_checked(ctx, inner),
        _ => Ok(None),
    }
}

/// Scope-aware expression typing for conditional-expression checks:
/// `sema::types::expr_type` has no scope context and resolves bare
/// names through implicit rules, mistyping locals and dummies. Names
/// go through the validator's scoped lookup instead; everything else
/// falls back to expr_type (whose recursion bottoms out in literals
/// and operators, which are scope-independent).
fn conditional_operand_type(
    ctx: &Ctx<'_>,
    expr: &crate::ast::expr::SpannedExpr,
) -> crate::sema::types::FortranType {
    match &expr.node {
        Expr::Name { name } => ctx
            .lookup(name)
            .and_then(|sym| sym.type_info.as_ref())
            .map(crate::sema::types::type_info_to_fortran_type)
            .unwrap_or(crate::sema::types::FortranType::Unknown),
        Expr::ParenExpr { inner } => conditional_operand_type(ctx, inner),
        _ => crate::sema::types::expr_type(expr, ctx.st),
    }
}

// ---- F2023 enumeration types (7.6.2) ----
//
// Enumeration types are a distinct TKR: no implicit conversion to or
// from INTEGER exists in either direction. All safety is frontend-only
// (values lower to i32 ordinals), so these checks are the only thing
// standing between a typo and a silently-working integer program.

/// Conservative enumeration classification of an expression. `Unknown`
/// suppresses diagnostics — only classify `NotEnum` when the type is
/// positively known, so untypeable expressions (intrinsic calls,
/// generics, components) never produce false positives.
#[derive(Clone, PartialEq)]
enum EnumClass {
    Enum(String),
    NotEnum,
    Unknown,
}

fn enum_class_of(ctx: &Ctx<'_>, expr: &crate::ast::expr::SpannedExpr) -> EnumClass {
    match &expr.node {
        Expr::Name { name } => match ctx.lookup(name) {
            Some(sym) if matches!(sym.kind, crate::sema::symtab::SymbolKind::EnumerationType) => {
                // A bare type name in expression position is an error
                // elsewhere; don't classify it as a value.
                EnumClass::Unknown
            }
            Some(sym) => match &sym.type_info {
                Some(TypeInfo::Enumeration(type_name)) => EnumClass::Enum(type_name.clone()),
                Some(_) => EnumClass::NotEnum,
                None => EnumClass::Unknown,
            },
            None => EnumClass::Unknown,
        },
        Expr::ParenExpr { inner } => enum_class_of(ctx, inner),
        Expr::ConditionalExpr { then_val, .. } => enum_class_of(ctx, then_val),
        Expr::IntegerLiteral { .. }
        | Expr::RealLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::ComplexLiteral { .. }
        | Expr::BozLiteral { .. } => EnumClass::NotEnum,
        // Operators never yield enumeration values (misuse of an
        // enumeration operand is flagged at the operator itself).
        Expr::BinaryOp { .. } | Expr::UnaryOp { .. } => EnumClass::NotEnum,
        Expr::FunctionCall { callee, args: _ } => match &callee.node {
            Expr::Name { name } => match ctx.lookup(name) {
                // Constructor color(n) — yields a value of the type.
                Some(sym)
                    if matches!(sym.kind, crate::sema::symtab::SymbolKind::EnumerationType) =>
                {
                    EnumClass::Enum(sym.name.clone())
                }
                // Element of an enumeration array, or a function whose
                // result is an enumeration type.
                Some(sym) => match &sym.type_info {
                    Some(TypeInfo::Enumeration(type_name)) => EnumClass::Enum(type_name.clone()),
                    _ => EnumClass::Unknown,
                },
                None => EnumClass::Unknown,
            },
            _ => EnumClass::Unknown,
        },
        _ => EnumClass::Unknown,
    }
}

fn check_enum_binary_op(
    ctx: &mut Ctx<'_>,
    span: Span,
    op: &crate::ast::expr::BinaryOp,
    left: &crate::ast::expr::SpannedExpr,
    right: &crate::ast::expr::SpannedExpr,
) {
    use crate::ast::expr::BinaryOp as B;
    let lc = enum_class_of(ctx, left);
    let rc = enum_class_of(ctx, right);
    if !matches!(lc, EnumClass::Enum(_)) && !matches!(rc, EnumClass::Enum(_)) {
        return;
    }
    match op {
        // All six relational ops are valid between values of the SAME
        // enumeration type (ordinal order, F2023 10.1.5.5.2).
        B::Eq | B::Ne | B::Lt | B::Le | B::Gt | B::Ge => match (lc, rc) {
            (EnumClass::Enum(a), EnumClass::Enum(b)) if a != b => {
                ctx.error(
                    span,
                    format!(
                        "operands of '{}' must have the same enumeration type \
                         ('{}' vs '{}')",
                        op, a, b
                    ),
                );
            }
            (EnumClass::Enum(a), EnumClass::NotEnum) | (EnumClass::NotEnum, EnumClass::Enum(a)) => {
                ctx.error(
                    span,
                    format!(
                        "cannot compare enumeration type '{}' with a \
                         non-enumeration value; convert with INT(v)",
                        a
                    ),
                );
            }
            _ => {}
        },
        // A user-defined operator could be legitimately overloaded for
        // an enumeration type; stay silent.
        B::Defined(_) => {}
        _ => {
            let name = match (&lc, &rc) {
                (EnumClass::Enum(a), _) => a.clone(),
                (_, EnumClass::Enum(b)) => b.clone(),
                _ => unreachable!(),
            };
            ctx.error(
                span,
                format!(
                    "operator '{}' is not defined for enumeration type '{}' \
                     (F2023 7.6.2); convert with INT(v)",
                    op, name
                ),
            );
        }
    }
}

fn check_enum_unary_op(
    ctx: &mut Ctx<'_>,
    span: Span,
    op: &crate::ast::expr::UnaryOp,
    operand: &crate::ast::expr::SpannedExpr,
) {
    if matches!(op, crate::ast::expr::UnaryOp::Defined(_)) {
        return;
    }
    if let EnumClass::Enum(name) = enum_class_of(ctx, operand) {
        ctx.error(
            span,
            format!(
                "unary operator '{}' is not defined for enumeration type '{}' \
                 (F2023 7.6.2); convert with INT(v)",
                op, name
            ),
        );
    }
}

/// R771 constructor `type-name(int-expr)`: exactly one integer argument;
/// a constant argument must be a valid 1-based ordinal.
fn check_enum_constructor(
    ctx: &mut Ctx<'_>,
    span: Span,
    type_name: &str,
    enumerator_count: usize,
    args: &[crate::ast::expr::Argument],
) {
    let one_int_expr = "the enumeration constructor takes exactly one integer expression";
    if args.len() != 1 {
        ctx.error(
            span,
            format!("{} ('{}(int-expr)')", one_int_expr, type_name),
        );
        return;
    }
    let crate::ast::expr::SectionSubscript::Element(arg) = &args[0].value else {
        ctx.error(
            span,
            format!("{} ('{}(int-expr)')", one_int_expr, type_name),
        );
        return;
    };
    if let EnumClass::Enum(arg_type) = enum_class_of(ctx, arg) {
        ctx.error(
            arg.span,
            format!(
                "the argument of the '{}' constructor must be an integer \
                 expression, not a value of enumeration type '{}'",
                type_name, arg_type
            ),
        );
        return;
    }
    if let Ok(Some(value)) = eval_const_int_expr_checked(ctx, arg) {
        if value.value < 1 || value.value > enumerator_count as i128 {
            ctx.error(
                arg.span,
                format!(
                    "value {} is out of range for enumeration type '{}' \
                     (valid ordinals are 1..{})",
                    value.value, type_name, enumerator_count
                ),
            );
        }
    }
}

/// Statement-level enumeration checks: intrinsic assignment requires
/// the same enumeration type on both sides, and list-directed I/O of
/// an enumeration value is invalid (F2023 7.6.2 — write INT(v) with an
/// I edit descriptor instead).
fn validate_stmt_enum_usage(ctx: &mut Ctx<'_>, stmt: &SpannedStmt) {
    fn check_list_directed_items(
        ctx: &mut Ctx<'_>,
        items: &[crate::ast::expr::SpannedExpr],
        verb: &str,
    ) {
        for item in items {
            if let EnumClass::Enum(name) = enum_class_of(ctx, item) {
                ctx.error(
                    item.span,
                    format!(
                        "list-directed {} of enumeration type '{}' is not \
                         allowed (F2023 7.6.2); {} INT(v) instead",
                        verb, name, verb
                    ),
                );
            }
        }
    }
    fn is_star(expr: &crate::ast::expr::SpannedExpr) -> bool {
        matches!(&expr.node, Expr::Name { name } if name == "*")
    }
    // fmt= is the second positional control or the FMT= keyword; a
    // bare `*` there means list-directed.
    fn io_is_list_directed(controls: &[crate::ast::stmt::IoControl]) -> bool {
        let mut positional = 0;
        for control in controls {
            match control.keyword.as_deref() {
                Some(kw) if kw.eq_ignore_ascii_case("fmt") => return is_star(&control.value),
                Some(_) => {}
                None => {
                    positional += 1;
                    if positional == 2 {
                        return is_star(&control.value);
                    }
                }
            }
        }
        false
    }

    match &stmt.node {
        Stmt::Assignment { target, value } => {
            let t = enum_class_of(ctx, target);
            let v = enum_class_of(ctx, value);
            match (t, v) {
                (EnumClass::Enum(a), EnumClass::Enum(b)) if a != b => {
                    ctx.error(
                        stmt.span,
                        format!(
                            "cannot assign a value of enumeration type '{}' to a \
                             variable of enumeration type '{}'",
                            b, a
                        ),
                    );
                }
                (EnumClass::Enum(a), EnumClass::NotEnum) => {
                    ctx.error(
                        stmt.span,
                        format!(
                            "cannot assign a non-enumeration value to a variable \
                             of enumeration type '{}'; use the constructor \
                             '{}(int-expr)' (F2023 7.6.2)",
                            a, a
                        ),
                    );
                }
                (EnumClass::NotEnum, EnumClass::Enum(b)) => {
                    ctx.error(
                        stmt.span,
                        format!(
                            "cannot assign a value of enumeration type '{}' to a \
                             non-enumeration variable; convert with INT(v)",
                            b
                        ),
                    );
                }
                _ => {}
            }
        }
        // Argument association: an enumeration actual requires a dummy
        // of the same enumeration type and vice versa (no implicit
        // INTEGER bridge — an integer actual against an enumeration
        // dummy read garbage before this check existed). Checked for
        // CALL to a uniquely-resolvable subroutine; anything ambiguous
        // (generics, externals without a visible body) is skipped.
        Stmt::Call { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return;
            };
            if name.eq_ignore_ascii_case("c_f_strpointer") {
                validate_c_f_strpointer(ctx, args);
                // Intrinsic — no user subroutine scope to walk below.
                return;
            }
            if name.eq_ignore_ascii_case("c_f_pointer") {
                validate_c_f_pointer(ctx, args);
                return;
            }
            let Some(arg_names) = ctx.lookup(name).and_then(|sym| {
                matches!(sym.kind, crate::sema::symtab::SymbolKind::Subroutine)
                    .then(|| sym.arg_names.clone())
            }) else {
                return;
            };
            let target = name.to_ascii_lowercase();
            let mut scopes = ctx.st.all_scopes().iter().filter(|scope| {
                matches!(&scope.kind, crate::sema::symtab::ScopeKind::Subroutine(n)
                    if n.eq_ignore_ascii_case(&target))
            });
            let (Some(callee_scope), None) = (scopes.next(), scopes.next()) else {
                return;
            };
            for (i, arg) in args.iter().enumerate() {
                let dummy_name = match arg.keyword.as_deref() {
                    Some(kw) => kw.to_ascii_lowercase(),
                    None => match arg_names.get(i) {
                        Some(n) => n.to_ascii_lowercase(),
                        None => continue,
                    },
                };
                // F2023 C1525: a `.NIL.` conditional-argument arm selects
                // the absent association — legal only against an OPTIONAL
                // dummy. Passing it to a required dummy hands the callee a
                // null it must dereference (PRESENT() would be false but
                // the storage is absent).
                if let crate::ast::expr::SectionSubscript::Element(actual) = &arg.value {
                    if expr_has_nil_arm(actual) {
                        let dummy_optional = callee_scope
                            .symbols
                            .get(&dummy_name)
                            .map(|sym| sym.attrs.optional)
                            .unwrap_or(false);
                        if !dummy_optional {
                            ctx.error(
                                actual.span,
                                format!(
                                    "a .NIL. conditional-argument arm requires an OPTIONAL \
                                     dummy, but '{dummy_name}' is not OPTIONAL (F2023 C1525)"
                                ),
                            );
                        }
                    }
                }
                let dummy_enum =
                    callee_scope
                        .symbols
                        .get(&dummy_name)
                        .and_then(|sym| match &sym.type_info {
                            Some(TypeInfo::Enumeration(n)) => Some(EnumClass::Enum(n.clone())),
                            Some(_) => Some(EnumClass::NotEnum),
                            None => None,
                        });
                let crate::ast::expr::SectionSubscript::Element(actual) = &arg.value else {
                    continue;
                };
                let actual_enum = enum_class_of(ctx, actual);
                match (dummy_enum, actual_enum) {
                    (Some(EnumClass::Enum(a)), EnumClass::Enum(b)) if a != b => {
                        ctx.error(
                            actual.span,
                            format!(
                                "actual argument of enumeration type '{}' is not \
                                 compatible with dummy '{}' of enumeration type '{}'",
                                b, dummy_name, a
                            ),
                        );
                    }
                    (Some(EnumClass::Enum(a)), EnumClass::NotEnum) => {
                        ctx.error(
                            actual.span,
                            format!(
                                "dummy argument '{}' has enumeration type '{}'; pass \
                                 a value of that type (constructor '{}(int-expr)')",
                                dummy_name, a, a
                            ),
                        );
                    }
                    (Some(EnumClass::NotEnum), EnumClass::Enum(b)) => {
                        ctx.error(
                            actual.span,
                            format!(
                                "actual argument of enumeration type '{}' is not \
                                 compatible with non-enumeration dummy '{}'; convert \
                                 with INT(v)",
                                b, dummy_name
                            ),
                        );
                    }
                    _ => {}
                }
            }
        }
        Stmt::Print { format, items } if is_star(format) => {
            check_list_directed_items(ctx, items, "output");
        }
        Stmt::Write { controls, items } if io_is_list_directed(controls) => {
            check_list_directed_items(ctx, items, "output");
        }
        Stmt::Read { controls, items } if io_is_list_directed(controls) => {
            check_list_directed_items(ctx, items, "input");
        }
        _ => {}
    }
}

/// Syntactic + symbol-table probe for an array-valued conditional arm:
/// a whole-array name, an array constructor, or a section (a call-form
/// expression with a range subscript). Conservative — anything it
/// misses is caught by the same garbage-descriptor class this guards,
/// so keep it in sync with the lowering when array merges land.
/// F2023 18.2.3.5 constraints on `CALL C_F_STRPOINTER(...)`. FSTRPTR must
/// be a deferred-length character pointer; NCHARS is required when the
/// source is a `type(c_ptr)` (the byte count cannot be inferred otherwise).
/// Only flags positively-determined violations — an unresolved actual is
/// left for the general resolver, never a false positive here.
fn validate_c_f_strpointer(ctx: &mut Ctx<'_>, args: &[crate::ast::expr::Argument]) {
    use crate::ast::expr::SectionSubscript;

    fn arg<'a>(
        args: &'a [crate::ast::expr::Argument],
        kw: &str,
        pos: usize,
    ) -> Option<&'a crate::ast::expr::SpannedExpr> {
        if let Some(a) = args.iter().find(|a| {
            a.keyword
                .as_deref()
                .is_some_and(|k| k.eq_ignore_ascii_case(kw))
        }) {
            if let SectionSubscript::Element(e) = &a.value {
                return Some(e);
            }
        }
        args.iter()
            .filter(|a| a.keyword.is_none())
            .nth(pos)
            .and_then(|a| match &a.value {
                SectionSubscript::Element(e) => Some(e),
                _ => None,
            })
    }

    let src = arg(args, "cstrarray", 0).or_else(|| arg(args, "cstrptr", 0));
    let fstrptr = arg(args, "fstrptr", 1);
    let nchars_present = arg(args, "nchars", 2).is_some();

    // FSTRPTR must be a character pointer (deferred-length, kind=c_char).
    if let Some(f) = fstrptr {
        if let Expr::Name { name } = &f.node {
            if let Some(sym) = ctx.lookup(name) {
                let is_char = matches!(sym.type_info, Some(TypeInfo::Character { .. }));
                let is_ptr = sym.attrs.pointer;
                if !(is_char && is_ptr) {
                    ctx.error(
                        f.span,
                        format!(
                            "C_F_STRPOINTER: FSTRPTR ('{name}') must be a deferred-length \
                             character (kind=c_char) pointer (F2023 18.2.3.5)"
                        ),
                    );
                }
            }
        }
    }

    // NCHARS is required when the source is a c_ptr: a scalar address has no
    // size, so the length cannot be bounded without it.
    if let Some(s) = src {
        if let Expr::Name { name } = &s.node {
            if let Some(sym) = ctx.lookup(name) {
                let is_cptr = matches!(
                    &sym.type_info,
                    Some(TypeInfo::Derived(n)) if n.eq_ignore_ascii_case("c_ptr")
                );
                if is_cptr && !nchars_present {
                    ctx.error(
                        s.span,
                        "C_F_STRPOINTER: NCHARS is required when the source is a \
                         type(c_ptr) (F2023 18.2.3.5)"
                            .to_string(),
                    );
                }
            }
        }
    }
}

/// F2023 C_F_POINTER LOWER constraints (gfortran.dg/c_f_pointer_shape_tests_8):
/// the optional LOWER argument must be INTEGER and rank 1, and its size must
/// match SHAPE. Only positively-determined violations are flagged.
fn validate_c_f_pointer(ctx: &mut Ctx<'_>, args: &[crate::ast::expr::Argument]) {
    use crate::ast::expr::SectionSubscript;

    fn arg<'a>(
        args: &'a [crate::ast::expr::Argument],
        kw: &str,
        pos: usize,
    ) -> Option<&'a crate::ast::expr::SpannedExpr> {
        if let Some(a) = args.iter().find(|a| {
            a.keyword
                .as_deref()
                .is_some_and(|k| k.eq_ignore_ascii_case(kw))
        }) {
            if let SectionSubscript::Element(e) = &a.value {
                return Some(e);
            }
        }
        args.iter()
            .filter(|a| a.keyword.is_none())
            .nth(pos)
            .and_then(|a| match &a.value {
                SectionSubscript::Element(e) => Some(e),
                _ => None,
            })
    }

    // Static rank of an actual: array constructor → 1, named array → its
    // declared rank. None when not determinable.
    fn rank_of(ctx: &Ctx<'_>, e: &crate::ast::expr::SpannedExpr) -> Option<usize> {
        match &e.node {
            Expr::ArrayConstructor { .. } => Some(1),
            Expr::Name { name } => ctx.lookup(name).map(|s| s.attrs.array_spec.len()),
            _ => None,
        }
    }
    // Static element count of a rank-1 actual. Only inline constructors give a
    // sound compile-time count without const-evaluating declared bounds; named
    // arrays return None and skip the conformance check (no false positives).
    fn static_len(_ctx: &Ctx<'_>, e: &crate::ast::expr::SpannedExpr) -> Option<usize> {
        match &e.node {
            Expr::ArrayConstructor { values, .. } => Some(values.len()),
            _ => None,
        }
    }

    let shape = arg(args, "shape", 2);
    let Some(lower) = arg(args, "lower", 3) else {
        return; // LOWER absent — nothing to check.
    };

    // LOWER must be INTEGER.
    let lower_ty = crate::sema::types::expr_type(lower, ctx.st);
    if !matches!(lower_ty, crate::sema::types::FortranType::Integer { .. }) {
        ctx.error(
            lower.span,
            "C_F_POINTER LOWER argument must be of type INTEGER (F2023)".to_string(),
        );
    }

    // LOWER must be rank 1.
    if let Some(r) = rank_of(ctx, lower) {
        if r != 1 {
            ctx.error(
                lower.span,
                format!("C_F_POINTER LOWER argument must be of rank 1, not rank {r} (F2023)"),
            );
        }
    }

    // LOWER size must match SHAPE size (conformance).
    if let (Some(s), Some(l)) = (
        shape.and_then(|s| static_len(ctx, s)),
        static_len(ctx, lower),
    ) {
        if s != l {
            ctx.error(
                lower.span,
                format!(
                    "C_F_POINTER LOWER has {l} element(s) but SHAPE has {s}; \
                     they must conform (F2023)"
                ),
            );
        }
    }
}

/// Two dummy/result types agree for separate-module-procedure matching
/// (F2008 C1418): same category, and same kind/rank where both are known.
/// Unknown kinds (`None`) are treated as compatible to avoid false
/// positives from `.amod`-loaded interfaces that don't preserve a kind.
fn smp_type_compatible(a: &TypeInfo, b: &TypeInfo) -> bool {
    fn kinds_ok(a: &Option<u8>, b: &Option<u8>) -> bool {
        matches!((a, b), (Some(x), Some(y)) if x == y) || a.is_none() || b.is_none()
    }
    match (a, b) {
        (TypeInfo::Integer { kind: k1 }, TypeInfo::Integer { kind: k2 })
        | (TypeInfo::Real { kind: k1 }, TypeInfo::Real { kind: k2 })
        | (TypeInfo::Complex { kind: k1 }, TypeInfo::Complex { kind: k2 })
        | (TypeInfo::Logical { kind: k1 }, TypeInfo::Logical { kind: k2 })
        | (TypeInfo::Character { kind: k1, .. }, TypeInfo::Character { kind: k2, .. }) => {
            kinds_ok(k1, k2)
        }
        (TypeInfo::Derived(n1), TypeInfo::Derived(n2))
        | (TypeInfo::Class(n1), TypeInfo::Class(n2)) => n1.eq_ignore_ascii_case(n2),
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

/// F2008 §12.6.2.5 / C1414/C1418: a separate module procedure body must
/// match its interface in the ancestor module — the interface must exist,
/// and the dummy arguments must agree in number, type, kind, and rank.
/// The interface association is enforced by the lowering regardless (it
/// injects the interface signature), so this is a pure diagnostic. Runs
/// for any procedure body carrying the MODULE prefix inside a submodule.
fn validate_smp_body(ctx: &mut Ctx<'_>, name: &str, prefix: &[Prefix], span: Span) {
    if !prefix.iter().any(|p| matches!(p, Prefix::Module)) {
        return;
    }
    let body_scope = ctx.scope_id;
    let Some(submod_id) = ctx.st.scope(body_scope).parent else {
        return;
    };
    if !matches!(ctx.st.scope(submod_id).kind, ScopeKind::Submodule(_)) {
        return;
    }
    let Some(parent_mod) = ctx
        .st
        .scope(submod_id)
        .use_associations
        .iter()
        .find(|u| u.is_submodule_access)
        .map(|u| u.source_scope)
    else {
        return;
    };

    // Locate the interface procedure scope in the ancestor module,
    // tolerating one Interface-block hop (as the lowering does).
    let proc_lc = name.to_lowercase();
    let iface = ctx.st.all_scopes().iter().find_map(|s| {
        let nm = match &s.kind {
            ScopeKind::Function(n) | ScopeKind::Subroutine(n) => n,
            _ => return None,
        };
        if !nm.eq_ignore_ascii_case(&proc_lc) {
            return None;
        }
        let p = s.parent?;
        if p == parent_mod
            || (matches!(ctx.st.scope(p).kind, ScopeKind::Interface)
                && ctx.st.scope(p).parent == Some(parent_mod))
        {
            Some(s.id)
        } else {
            None
        }
    });
    // No interface scope located. This is NOT necessarily an error: a
    // separate module procedure can implement a specific inside a GENERIC
    // interface, whose members are loaded from the parent `.amod` as
    // NamedInterface symbols without a per-specific proc scope. Diagnosing
    // "no matching interface" here would false-positive on those valid
    // SMPs (cli_driver submodule_dispatching_private_parent_generic_…),
    // so bail quietly — the signature checks below only run when a real
    // interface scope is found, which keeps them false-positive-safe. A
    // robust truly-missing-interface check is deferred (see noted_items).
    let Some(iface) = iface else {
        return;
    };

    let iface_args = ctx.st.scope(iface).arg_order.clone();
    let body_args = ctx.st.scope(body_scope).arg_order.clone();
    if iface_args.len() != body_args.len() {
        ctx.error(
            span,
            format!(
                "separate module procedure '{name}' has {} dummy argument(s) but its \
                 interface declares {} (F2008 C1418)",
                body_args.len(),
                iface_args.len()
            ),
        );
        return;
    }
    for (ia, ba) in iface_args.iter().zip(body_args.iter()) {
        let isym = ctx.st.scope(iface).symbols.get(ia).cloned();
        let bsym = ctx.st.scope(body_scope).symbols.get(ba).cloned();
        let (Some(isym), Some(bsym)) = (isym, bsym) else {
            continue;
        };
        if let (Some(it), Some(bt)) = (&isym.type_info, &bsym.type_info) {
            if !smp_type_compatible(it, bt) {
                ctx.error(
                    span,
                    format!(
                        "dummy argument '{ba}' of separate module procedure '{name}' does \
                         not match its interface in the ancestor module (F2008 C1418: \
                         type, kind, and rank must agree)"
                    ),
                );
            }
        }
        if isym.attrs.array_spec.len() != bsym.attrs.array_spec.len() {
            ctx.error(
                span,
                format!(
                    "dummy argument '{ba}' of separate module procedure '{name}' has rank \
                     {} but its interface declares rank {} (F2008 C1418)",
                    bsym.attrs.array_spec.len(),
                    isym.attrs.array_spec.len()
                ),
            );
        }
    }
}

/// True if a conditional actual argument has a `.NIL.` arm anywhere in its
/// (possibly chained) conditional tree. Such an argument selects the absent
/// association, which F2023 C1525 permits only for an OPTIONAL dummy.
fn expr_has_nil_arm(expr: &crate::ast::expr::SpannedExpr) -> bool {
    match &expr.node {
        Expr::NilArgument => true,
        Expr::ConditionalExpr {
            then_val, else_val, ..
        } => expr_has_nil_arm(then_val) || expr_has_nil_arm(else_val),
        _ => false,
    }
}

fn conditional_arm_is_arraylike(ctx: &Ctx<'_>, expr: &crate::ast::expr::SpannedExpr) -> bool {
    match &expr.node {
        Expr::ArrayConstructor { .. } => true,
        Expr::Name { name } => ctx
            .lookup(name)
            .is_some_and(|sym| !sym.attrs.array_spec.is_empty()),
        Expr::FunctionCall { callee, args } => {
            // A range subscript makes it a section — unless the base is
            // a CHARACTER scalar, where `c(1:3)` is a SUBSTRING (scalar,
            // legal as an arm; gfortran's conditional_1 relies on it).
            // An array-returning function (declared result_rank > 0) is
            // array-valued even with scalar args.
            let base_is_char_scalar = matches!(
                &callee.node,
                Expr::Name { name } if ctx.lookup(name).is_some_and(|sym| {
                    sym.attrs.array_spec.is_empty()
                        && matches!(
                            sym.type_info,
                            Some(crate::sema::symtab::TypeInfo::Character { .. })
                        )
                })
            );
            (!base_is_char_scalar
                && args.iter().any(|arg| {
                    !matches!(arg.value, crate::ast::expr::SectionSubscript::Element(_))
                }))
                || matches!(
                    &callee.node,
                    Expr::Name { name } if ctx
                        .lookup(name)
                        .is_some_and(|sym| sym.attrs.result_rank > 0)
                )
        }
        Expr::ParenExpr { inner } => conditional_arm_is_arraylike(ctx, inner),
        _ => false,
    }
}

fn validate_const_int_expr_tree(ctx: &mut Ctx<'_>, expr: &crate::ast::expr::SpannedExpr) {
    match &expr.node {
        Expr::NilArgument => {
            // Reaching a bare .NIL. here means it sits outside a
            // conditional-argument arm (the call-argument walk strips
            // the legal positions before recursing).
            ctx.error(
                expr.span,
                ".NIL. is only valid as an arm of a conditional actual argument",
            );
        }
        Expr::ConditionalExpr {
            cond,
            then_val,
            else_val,
        } => {
            ctx.require_std(expr.span, FortranStandard::F2023, "conditional expression");
            let cond_ty = conditional_operand_type(ctx, cond);
            if !matches!(
                cond_ty,
                crate::sema::types::FortranType::Logical { .. }
                    | crate::sema::types::FortranType::Unknown
            ) {
                ctx.error(
                    cond.span,
                    "condition in a conditional expression must be a scalar LOGICAL",
                );
            }
            // Arms must share declared type and kind; character lengths
            // may differ (gfortran conditional_1.f90 assigns mixed-length
            // arms to a deferred-length allocatable).
            let t_ty = conditional_operand_type(ctx, then_val);
            let e_ty = conditional_operand_type(ctx, else_val);
            let compatible = match (&t_ty, &e_ty) {
                (crate::sema::types::FortranType::Unknown, _)
                | (_, crate::sema::types::FortranType::Unknown) => true,
                (
                    crate::sema::types::FortranType::Character { kind: k1, .. },
                    crate::sema::types::FortranType::Character { kind: k2, .. },
                ) => k1 == k2,
                (a, b) => a == b,
            };
            if !compatible {
                ctx.error(
                    else_val.span,
                    format!(
                        "arms of a conditional expression must have the same declared \
                         type and kind (then-arm is {:?}, else-arm is {:?})",
                        t_ty, e_ty
                    ),
                );
            }
            // Array-valued arms only lower when this conditional is the
            // direct RHS of an array assignment — lower_array_conditional_assign
            // branches per arm and reuses the ordinary assignment path.
            // Anywhere else the merge has no array-descriptor lowering, so
            // reject loudly (it built a corrupt merge before this guard).
            let allow_array_arms = ctx.allow_array_cond_rhs;
            if !allow_array_arms {
                for arm in [then_val, else_val] {
                    if conditional_arm_is_arraylike(ctx, arm) {
                        ctx.error(
                            arm.span,
                            "conditional expressions with array-valued arms are only \
                             supported as the right-hand side of an assignment; assign \
                             through an IF construct instead",
                        );
                    }
                }
            }
            for arm in [then_val, else_val] {
                if matches!(arm.node, Expr::NilArgument) && !ctx.in_call_arg {
                    ctx.error(
                        arm.span,
                        ".NIL. is only valid as an arm of a conditional actual \
                         argument in a CALL statement",
                    );
                }
            }
            ctx.allow_array_cond_rhs = false;
            validate_const_int_expr_tree(ctx, cond);
            for arm in [then_val, else_val] {
                if !matches!(arm.node, Expr::NilArgument) {
                    // Propagate the allowance only into a directly-nested
                    // conditional arm (a chained `c1 ? a : c2 ? b : c`),
                    // which lowers through the same per-arm branch. An array
                    // conditional buried in a larger arm expression stays
                    // rejected.
                    ctx.allow_array_cond_rhs =
                        allow_array_arms && matches!(arm.node, Expr::ConditionalExpr { .. });
                    validate_const_int_expr_tree(ctx, arm);
                }
            }
            ctx.allow_array_cond_rhs = false;
        }
        Expr::IntegerLiteral { .. } => {
            if let Err(diag) = eval_const_int_expr_checked(ctx, expr) {
                ctx.error(diag.span, diag.msg);
            }
        }
        Expr::UnaryOp { op, operand } => {
            check_enum_unary_op(ctx, expr.span, op, operand);
            if matches!(
                op,
                crate::ast::expr::UnaryOp::Plus | crate::ast::expr::UnaryOp::Minus
            ) {
                match eval_const_int_expr_checked(ctx, expr) {
                    Ok(Some(_)) => {}
                    Err(diag) => ctx.error(diag.span, diag.msg),
                    Ok(None) => validate_const_int_expr_tree(ctx, operand),
                }
            } else {
                validate_const_int_expr_tree(ctx, operand);
            }
        }
        Expr::BinaryOp { op, left, right } => {
            check_enum_binary_op(ctx, expr.span, op, left, right);
            if matches!(
                op,
                crate::ast::expr::BinaryOp::Add
                    | crate::ast::expr::BinaryOp::Sub
                    | crate::ast::expr::BinaryOp::Mul
                    | crate::ast::expr::BinaryOp::Div
                    | crate::ast::expr::BinaryOp::Pow
            ) {
                match eval_const_int_expr_checked(ctx, expr) {
                    Ok(Some(_)) => {}
                    Err(diag) => ctx.error(diag.span, diag.msg),
                    Ok(None) => {
                        validate_const_int_expr_tree(ctx, left);
                        validate_const_int_expr_tree(ctx, right);
                    }
                }
            } else {
                validate_const_int_expr_tree(ctx, left);
                validate_const_int_expr_tree(ctx, right);
            }
        }
        Expr::FunctionCall { callee, args } => {
            validate_const_int_expr_tree(ctx, callee);
            if let Expr::Name { name } = &callee.node {
                let enum_ctor = ctx.lookup(name).and_then(|sym| {
                    matches!(sym.kind, crate::sema::symtab::SymbolKind::EnumerationType)
                        .then(|| (sym.name.clone(), sym.arg_names.len()))
                });
                if let Some((type_name, count)) = enum_ctor {
                    check_enum_constructor(ctx, expr.span, &type_name, count, args);
                }
                // F2023 16.9.181: SELECTED_LOGICAL_KIND is f2023-only
                // (gfortran rejects it under -std=f2018 as undeclared);
                // gate it when the name resolves to the intrinsic (a
                // user procedure of the same name shadows it and is
                // exempt). The degree/half-rev trig functions are NOT
                // gated — gfortran ships them as longstanding extensions.
                if name.eq_ignore_ascii_case("selected_logical_kind") && ctx.lookup(name).is_none()
                {
                    ctx.require_std(expr.span, FortranStandard::F2023, "SELECTED_LOGICAL_KIND");
                }
                // Arity gate: only when every arg is an Element — a
                // Range subscript makes this a section or substring
                // of a variable, never an intrinsic reference.
                if args
                    .iter()
                    .all(|a| matches!(a.value, crate::ast::expr::SectionSubscript::Element(_)))
                {
                    check_intrinsic_call_arity(ctx, expr.span, name, args.len(), false);
                }
            }
            // F2023 conditional arguments in FUNCTION references select the
            // argument association per arm on the fn-call lowering path
            // (lower_call_arg_maybe_conditional), the same as CALL.
            for arg in args {
                validate_const_int_subscript(ctx, &arg.value);
            }
        }
        Expr::ArrayConstructor { values, .. } => {
            for value in values {
                validate_const_int_ac_value(ctx, value);
            }
        }
        Expr::ComponentAccess { base, .. } => {
            validate_component_access(ctx, expr);
            validate_const_int_expr_tree(ctx, base);
        }
        Expr::ComplexLiteral { real, imag } => {
            validate_const_int_expr_tree(ctx, real);
            validate_const_int_expr_tree(ctx, imag);
        }
        Expr::ParenExpr { inner } => validate_const_int_expr_tree(ctx, inner),
        Expr::Name { .. }
        | Expr::RealLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::BozLiteral { .. } => {}
    }
}

fn validate_const_int_subscript(ctx: &mut Ctx<'_>, value: &crate::ast::expr::SectionSubscript) {
    match value {
        crate::ast::expr::SectionSubscript::Element(expr) => {
            validate_const_int_expr_tree(ctx, expr)
        }
        crate::ast::expr::SectionSubscript::Range { start, end, stride } => {
            if let Some(expr) = start {
                validate_const_int_expr_tree(ctx, expr);
            }
            if let Some(expr) = end {
                validate_const_int_expr_tree(ctx, expr);
            }
            if let Some(expr) = stride {
                validate_const_int_expr_tree(ctx, expr);
            }
        }
    }
}

fn validate_const_int_ac_value(ctx: &mut Ctx<'_>, value: &crate::ast::expr::AcValue) {
    match value {
        crate::ast::expr::AcValue::Expr(expr) => validate_const_int_expr_tree(ctx, expr),
        crate::ast::expr::AcValue::ImpliedDo(loop_) => {
            for nested in &loop_.values {
                validate_const_int_ac_value(ctx, nested);
            }
            validate_const_int_expr_tree(ctx, &loop_.start);
            validate_const_int_expr_tree(ctx, &loop_.end);
            if let Some(step) = &loop_.step {
                validate_const_int_expr_tree(ctx, step);
            }
        }
    }
}

fn validate_const_int_array_spec(ctx: &mut Ctx<'_>, spec: &crate::ast::decl::ArraySpec) {
    match spec {
        crate::ast::decl::ArraySpec::Explicit { lower, upper } => {
            if let Some(lower) = lower {
                validate_const_int_expr_tree(ctx, lower);
            }
            validate_const_int_expr_tree(ctx, upper);
        }
        crate::ast::decl::ArraySpec::AssumedShape { lower }
        | crate::ast::decl::ArraySpec::AssumedSize { lower } => {
            if let Some(lower) = lower {
                validate_const_int_expr_tree(ctx, lower);
            }
        }
        crate::ast::decl::ArraySpec::Deferred | crate::ast::decl::ArraySpec::AssumedRank => {}
    }
}

fn validate_const_int_type_spec(ctx: &mut Ctx<'_>, type_spec: &TypeSpec) {
    match type_spec {
        TypeSpec::Integer(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Integer(Some(crate::ast::decl::KindSelector::Star(expr)))
        | TypeSpec::Real(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Real(Some(crate::ast::decl::KindSelector::Star(expr)))
        | TypeSpec::Complex(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Complex(Some(crate::ast::decl::KindSelector::Star(expr)))
        | TypeSpec::Logical(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Logical(Some(crate::ast::decl::KindSelector::Star(expr))) => {
            validate_const_int_expr_tree(ctx, expr);
        }
        TypeSpec::Character(Some(sel)) => {
            if let Some(crate::ast::decl::LenSpec::Expr(expr)) = &sel.len {
                validate_const_int_expr_tree(ctx, expr);
            }
            if let Some(kind) = &sel.kind {
                validate_const_int_expr_tree(ctx, kind);
            }
        }
        _ => {}
    }
}

fn validate_decl_const_int_exprs(ctx: &mut Ctx<'_>, decl: &crate::ast::decl::SpannedDecl) {
    match &decl.node {
        Decl::TypeDecl {
            type_spec,
            attrs,
            entities,
        } => {
            validate_const_int_type_spec(ctx, type_spec);
            for attr in attrs {
                if let Attribute::Dimension(specs) = attr {
                    for spec in specs {
                        validate_const_int_array_spec(ctx, spec);
                    }
                }
            }
            for entity in entities {
                if let Some(specs) = &entity.array_spec {
                    for spec in specs {
                        validate_const_int_array_spec(ctx, spec);
                    }
                }
                if let Some(crate::ast::decl::LenSpec::Expr(expr)) = &entity.char_len {
                    validate_const_int_expr_tree(ctx, expr);
                }
                if let Some(init) = &entity.init {
                    validate_const_int_expr_tree(ctx, init);
                }
                if let Some(init) = &entity.ptr_init {
                    validate_const_int_expr_tree(ctx, init);
                }
            }
        }
        Decl::ParameterStmt { pairs } => {
            for (_, expr) in pairs {
                validate_const_int_expr_tree(ctx, expr);
            }
        }
        Decl::EquivalenceStmt { groups } => {
            for group in groups {
                for expr in group {
                    validate_const_int_expr_tree(ctx, expr);
                }
            }
        }
        Decl::DataStmt { sets } => {
            for set in sets {
                for expr in &set.objects {
                    validate_const_int_expr_tree(ctx, expr);
                }
                for value in &set.values {
                    match value {
                        crate::ast::decl::DataValue::Expr(expr) => {
                            validate_const_int_expr_tree(ctx, expr);
                        }
                        crate::ast::decl::DataValue::Repeat { count, value } => {
                            validate_const_int_expr_tree(ctx, count);
                            validate_const_int_expr_tree(ctx, value);
                        }
                    }
                }
            }
        }
        Decl::EnumDef { enumerators, .. } => {
            for (_, expr) in enumerators {
                if let Some(expr) = expr {
                    validate_const_int_expr_tree(ctx, expr);
                }
            }
        }
        _ => {}
    }
}

/// Collect the (lowercased) names of all variables referenced anywhere in
/// an expression tree. Function/array-callee names are included too — a
/// harmless superset for the LOCAL-locality check, which only cares about
/// names that also appear in a locality-spec.
fn collect_expr_var_names(
    expr: &crate::ast::expr::SpannedExpr,
    out: &mut std::collections::HashSet<String>,
) {
    use crate::ast::expr::SectionSubscript;
    match &expr.node {
        Expr::Name { name } => {
            out.insert(name.to_lowercase());
        }
        Expr::ComponentAccess { base, .. } => collect_expr_var_names(base, out),
        Expr::UnaryOp { operand, .. } => collect_expr_var_names(operand, out),
        Expr::BinaryOp { left, right, .. } => {
            collect_expr_var_names(left, out);
            collect_expr_var_names(right, out);
        }
        Expr::ComplexLiteral { real, imag } => {
            collect_expr_var_names(real, out);
            collect_expr_var_names(imag, out);
        }
        Expr::FunctionCall { callee, args } => {
            collect_expr_var_names(callee, out);
            for arg in args {
                match &arg.value {
                    SectionSubscript::Element(e) => collect_expr_var_names(e, out),
                    SectionSubscript::Range { start, end, stride } => {
                        for e in [start, end, stride].into_iter().flatten() {
                            collect_expr_var_names(e, out);
                        }
                    }
                }
            }
        }
        Expr::ArrayConstructor { values, .. } => {
            for v in values {
                collect_ac_value_var_names(v, out);
            }
        }
        Expr::ParenExpr { inner } => collect_expr_var_names(inner, out),
        Expr::ConditionalExpr {
            cond,
            then_val,
            else_val,
        } => {
            collect_expr_var_names(cond, out);
            collect_expr_var_names(then_val, out);
            collect_expr_var_names(else_val, out);
        }
        _ => {}
    }
}

fn collect_ac_value_var_names(
    v: &crate::ast::expr::AcValue,
    out: &mut std::collections::HashSet<String>,
) {
    use crate::ast::expr::AcValue;
    match v {
        AcValue::Expr(e) => collect_expr_var_names(e, out),
        AcValue::ImpliedDo(ido) => {
            // ido.var is a loop-local binding, not an outer reference.
            collect_expr_var_names(&ido.start, out);
            collect_expr_var_names(&ido.end, out);
            if let Some(step) = &ido.step {
                collect_expr_var_names(step, out);
            }
            for inner in &ido.values {
                collect_ac_value_var_names(inner, out);
            }
        }
    }
}

fn validate_stmt_const_int_exprs(ctx: &mut Ctx<'_>, stmt: &SpannedStmt) {
    match &stmt.node {
        Stmt::Assignment { target, value } | Stmt::PointerAssignment { target, value } => {
            validate_const_int_expr_tree(ctx, target);
            // F2023: an array-valued conditional as the assignment RHS lowers
            // via a per-arm branch, so allow array arms there (but only for
            // a true assignment, not a pointer assignment).
            let allow = matches!(stmt.node, Stmt::Assignment { .. })
                && matches!(value.node, Expr::ConditionalExpr { .. });
            ctx.allow_array_cond_rhs = allow;
            validate_const_int_expr_tree(ctx, value);
            ctx.allow_array_cond_rhs = false;
        }
        Stmt::IfConstruct {
            condition,
            else_ifs,
            ..
        } => {
            validate_const_int_expr_tree(ctx, condition);
            for (expr, _) in else_ifs {
                validate_const_int_expr_tree(ctx, expr);
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
        } => validate_const_int_expr_tree(ctx, condition),
        Stmt::SelectCase {
            selector, cases, ..
        } => {
            validate_const_int_expr_tree(ctx, selector);
            for case in cases {
                for selector in &case.selectors {
                    match selector {
                        CaseSelector::Value(expr) => validate_const_int_expr_tree(ctx, expr),
                        CaseSelector::Range { low, high } => {
                            if let Some(low) = low {
                                validate_const_int_expr_tree(ctx, low);
                            }
                            if let Some(high) = high {
                                validate_const_int_expr_tree(ctx, high);
                            }
                        }
                        CaseSelector::Default => {}
                    }
                }
            }
        }
        Stmt::DoLoop {
            start, end, step, ..
        } => {
            if let Some(start) = start {
                validate_const_int_expr_tree(ctx, start);
            }
            if let Some(end) = end {
                validate_const_int_expr_tree(ctx, end);
            }
            if let Some(step) = step {
                validate_const_int_expr_tree(ctx, step);
                if let Ok(Some(value)) = eval_const_int_expr_checked(ctx, step) {
                    if value.value == 0 {
                        ctx.error(step.span, "DO step must not be zero");
                    }
                }
            }
        }
        Stmt::DoConcurrent {
            controls,
            mask,
            locality,
            ..
        } => {
            for control in controls {
                validate_const_int_expr_tree(ctx, &control.start);
                validate_const_int_expr_tree(ctx, &control.end);
                if let Some(step) = &control.step {
                    validate_const_int_expr_tree(ctx, step);
                }
            }
            if let Some(mask) = mask {
                validate_const_int_expr_tree(ctx, mask);
            }
            // F2023 C1133: a variable referenced in the concurrent-header
            // (loop bounds, step, mask) must not appear in a LOCAL
            // locality-spec — a LOCAL variable is undefined on entry, so
            // reading it in the header is meaningless. (LOCAL_INIT is
            // initialized from the outer scope, so it is exempt.)
            let mut header_names = std::collections::HashSet::new();
            for control in controls {
                collect_expr_var_names(&control.start, &mut header_names);
                collect_expr_var_names(&control.end, &mut header_names);
                if let Some(step) = &control.step {
                    collect_expr_var_names(step, &mut header_names);
                }
            }
            if let Some(mask) = mask {
                collect_expr_var_names(mask, &mut header_names);
            }
            for spec in locality {
                if let crate::ast::stmt::LocalitySpec::Local(vars) = spec {
                    for v in vars {
                        if header_names.contains(&v.to_lowercase()) {
                            ctx.error(
                                stmt.span,
                                format!(
                                    "variable '{v}' referenced in the concurrent-header \
                                     must not appear in a LOCAL locality-spec (F2023 C1133)"
                                ),
                            );
                        }
                    }
                }
            }
        }
        Stmt::WhereConstruct {
            mask, elsewhere, ..
        } => {
            validate_const_int_expr_tree(ctx, mask);
            for (maybe_mask, _) in elsewhere {
                if let Some(mask) = maybe_mask {
                    validate_const_int_expr_tree(ctx, mask);
                }
            }
        }
        Stmt::ForallConstruct { specs, mask, .. } | Stmt::ForallStmt { specs, mask, .. } => {
            for spec in specs {
                validate_const_int_expr_tree(ctx, &spec.start);
                validate_const_int_expr_tree(ctx, &spec.end);
                if let Some(step) = &spec.step {
                    validate_const_int_expr_tree(ctx, step);
                }
            }
            if let Some(mask) = mask {
                validate_const_int_expr_tree(ctx, mask);
            }
        }
        Stmt::Associate { assocs, .. } => {
            for (_, expr) in assocs {
                validate_const_int_expr_tree(ctx, expr);
            }
        }
        Stmt::Stop { code, .. } | Stmt::ErrorStop { code, .. } | Stmt::Return { value: code } => {
            if let Some(code) = code {
                validate_const_int_expr_tree(ctx, code);
            }
        }
        Stmt::Write { controls, items }
        | Stmt::Read { controls, items }
        | Stmt::Inquire {
            specs: controls,
            items,
        } => {
            for control in controls {
                validate_const_int_expr_tree(ctx, &control.value);
            }
            for item in items {
                validate_const_int_expr_tree(ctx, item);
            }
        }
        Stmt::Open { specs }
        | Stmt::Close { specs }
        | Stmt::Rewind { specs }
        | Stmt::Backspace { specs }
        | Stmt::Endfile { specs }
        | Stmt::Flush { specs }
        | Stmt::Wait { specs } => {
            for spec in specs {
                validate_const_int_expr_tree(ctx, &spec.value);
            }
        }
        Stmt::Allocate { items, opts, .. } | Stmt::Deallocate { items, opts } => {
            for item in items {
                validate_const_int_expr_tree(ctx, item);
            }
            for opt in opts {
                validate_const_int_expr_tree(ctx, &opt.value);
            }
        }
        Stmt::Nullify { items } => {
            for item in items {
                validate_const_int_expr_tree(ctx, item);
            }
        }
        Stmt::Call { callee, args } => {
            validate_const_int_expr_tree(ctx, callee);
            if let Expr::Name { name } = &callee.node {
                check_intrinsic_call_arity(ctx, stmt.span, name, args.len(), true);
            }
            let saved = ctx.in_call_arg;
            ctx.in_call_arg = true;
            for arg in args {
                validate_const_int_subscript(ctx, &arg.value);
            }
            ctx.in_call_arg = saved;
        }
        Stmt::Print { items, .. } => {
            for item in items {
                validate_const_int_expr_tree(ctx, item);
            }
        }
        Stmt::Block { .. }
        | Stmt::Declaration(_)
        | Stmt::Exit { .. }
        | Stmt::Cycle { .. }
        | Stmt::Goto { .. }
        | Stmt::Continue { .. }
        | Stmt::Format { .. }
        | Stmt::Labeled { .. }
        | Stmt::Namelist { .. } => {}
    }
}

fn warn_legacy_feature(ctx: &mut Ctx<'_>, span: Span, feature: &str) {
    if ctx.warn_pedantic || ctx.warn_deprecated {
        ctx.warning(span, format!("{} is an obsolescent feature", feature));
    }
}

fn decl_attrs_contain(attrs: &[Attribute], needle: Attribute) -> bool {
    attrs.contains(&needle)
}

fn is_deferred_char_pointer_component(type_spec: &TypeSpec, attrs: &[Attribute]) -> bool {
    decl_attrs_contain(attrs, Attribute::Pointer)
        && matches!(
            type_spec,
            TypeSpec::Character(Some(sel))
                if matches!(&sel.len, Some(crate::ast::decl::LenSpec::Colon))
        )
}

fn validate_unsupported_component_forms(
    ctx: &mut Ctx<'_>,
    components: &[crate::ast::decl::SpannedDecl],
) {
    let _ = (ctx, components);
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
            let target = name.clone().unwrap_or_else(|| "main".into());
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
        ProgramUnit::Submodule { name, .. } => {
            let n = name.clone();
            (
                Box::new(
                    move |k| matches!(k, ScopeKind::Submodule(ref m) if m.eq_ignore_ascii_case(&n)),
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

#[derive(Default)]
struct ProcedureReferenceFacts {
    references: Vec<(String, Span)>,
    calls: HashSet<String>,
}

fn collect_reference_subscript(
    subscript: &crate::ast::expr::SectionSubscript,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    match subscript {
        crate::ast::expr::SectionSubscript::Element(expr) => {
            collect_reference_expr(expr, shadowed, facts);
        }
        crate::ast::expr::SectionSubscript::Range { start, end, stride } => {
            for expr in [start, end, stride].into_iter().flatten() {
                collect_reference_expr(expr, shadowed, facts);
            }
        }
    }
}

fn collect_reference_ac_value(
    value: &crate::ast::expr::AcValue,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    match value {
        crate::ast::expr::AcValue::Expr(expr) => collect_reference_expr(expr, shadowed, facts),
        crate::ast::expr::AcValue::ImpliedDo(loop_) => {
            collect_reference_expr(&loop_.start, shadowed, facts);
            collect_reference_expr(&loop_.end, shadowed, facts);
            if let Some(step) = &loop_.step {
                collect_reference_expr(step, shadowed, facts);
            }
            let mut nested_shadowed = shadowed.clone();
            nested_shadowed.insert(loop_.var.to_lowercase());
            for nested in &loop_.values {
                collect_reference_ac_value(nested, &nested_shadowed, facts);
            }
        }
    }
}

fn collect_reference_expr(
    expr: &crate::ast::expr::SpannedExpr,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    match &expr.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            if !shadowed.contains(&key) {
                facts.references.push((key, expr.span));
            }
        }
        Expr::UnaryOp { operand, .. } => collect_reference_expr(operand, shadowed, facts),
        Expr::BinaryOp { left, right, .. } => {
            collect_reference_expr(left, shadowed, facts);
            collect_reference_expr(right, shadowed, facts);
        }
        Expr::ComplexLiteral { real, imag } => {
            collect_reference_expr(real, shadowed, facts);
            collect_reference_expr(imag, shadowed, facts);
        }
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                if !shadowed.contains(&key) {
                    facts.calls.insert(key);
                }
            }
            collect_reference_expr(callee, shadowed, facts);
            for arg in args {
                collect_reference_subscript(&arg.value, shadowed, facts);
            }
        }
        Expr::ArrayConstructor { values, .. } => {
            for value in values {
                collect_reference_ac_value(value, shadowed, facts);
            }
        }
        Expr::ComponentAccess { base, .. } => collect_reference_expr(base, shadowed, facts),
        Expr::ParenExpr { inner } => collect_reference_expr(inner, shadowed, facts),
        Expr::ConditionalExpr {
            cond,
            then_val,
            else_val,
        } => {
            collect_reference_expr(cond, shadowed, facts);
            collect_reference_expr(then_val, shadowed, facts);
            collect_reference_expr(else_val, shadowed, facts);
        }
        Expr::IntegerLiteral { .. }
        | Expr::RealLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::BozLiteral { .. }
        | Expr::NilArgument => {}
    }
}

fn collect_reference_array_spec(
    spec: &crate::ast::decl::ArraySpec,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    match spec {
        crate::ast::decl::ArraySpec::Explicit { lower, upper } => {
            if let Some(lower) = lower {
                collect_reference_expr(lower, shadowed, facts);
            }
            collect_reference_expr(upper, shadowed, facts);
        }
        crate::ast::decl::ArraySpec::AssumedShape { lower }
        | crate::ast::decl::ArraySpec::AssumedSize { lower } => {
            if let Some(lower) = lower {
                collect_reference_expr(lower, shadowed, facts);
            }
        }
        crate::ast::decl::ArraySpec::Deferred | crate::ast::decl::ArraySpec::AssumedRank => {}
    }
}

fn collect_reference_type_spec(
    type_spec: &TypeSpec,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    match type_spec {
        TypeSpec::Integer(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Integer(Some(crate::ast::decl::KindSelector::Star(expr)))
        | TypeSpec::Real(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Real(Some(crate::ast::decl::KindSelector::Star(expr)))
        | TypeSpec::Complex(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Complex(Some(crate::ast::decl::KindSelector::Star(expr)))
        | TypeSpec::Logical(Some(crate::ast::decl::KindSelector::Expr(expr)))
        | TypeSpec::Logical(Some(crate::ast::decl::KindSelector::Star(expr))) => {
            collect_reference_expr(expr, shadowed, facts);
        }
        TypeSpec::Character(Some(selector)) => {
            if let Some(crate::ast::decl::LenSpec::Expr(expr)) = &selector.len {
                collect_reference_expr(expr, shadowed, facts);
            }
            if let Some(kind) = &selector.kind {
                collect_reference_expr(kind, shadowed, facts);
            }
        }
        _ => {}
    }
}

fn collect_reference_decl(
    decl: &SpannedDecl,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    match &decl.node {
        Decl::TypeDecl {
            type_spec,
            attrs,
            entities,
        } => {
            collect_reference_type_spec(type_spec, shadowed, facts);
            for attr in attrs {
                if let Attribute::Dimension(specs) = attr {
                    for spec in specs {
                        collect_reference_array_spec(spec, shadowed, facts);
                    }
                }
            }
            for entity in entities {
                if let Some(specs) = &entity.array_spec {
                    for spec in specs {
                        collect_reference_array_spec(spec, shadowed, facts);
                    }
                }
                if let Some(crate::ast::decl::LenSpec::Expr(expr)) = &entity.char_len {
                    collect_reference_expr(expr, shadowed, facts);
                }
                if let Some(init) = &entity.init {
                    collect_reference_expr(init, shadowed, facts);
                }
                if let Some(init) = &entity.ptr_init {
                    collect_reference_expr(init, shadowed, facts);
                }
            }
        }
        Decl::ParameterStmt { pairs } => {
            for (_, expr) in pairs {
                collect_reference_expr(expr, shadowed, facts);
            }
        }
        Decl::EquivalenceStmt { groups } => {
            for expr in groups.iter().flatten() {
                collect_reference_expr(expr, shadowed, facts);
            }
        }
        Decl::DataStmt { sets } => {
            for set in sets {
                for expr in &set.objects {
                    collect_reference_expr(expr, shadowed, facts);
                }
                for value in &set.values {
                    match value {
                        crate::ast::decl::DataValue::Expr(expr) => {
                            collect_reference_expr(expr, shadowed, facts);
                        }
                        crate::ast::decl::DataValue::Repeat { count, value } => {
                            collect_reference_expr(count, shadowed, facts);
                            collect_reference_expr(value, shadowed, facts);
                        }
                    }
                }
            }
        }
        Decl::EnumDef { enumerators, .. } => {
            for (_, expr) in enumerators {
                if let Some(expr) = expr {
                    collect_reference_expr(expr, shadowed, facts);
                }
            }
        }
        Decl::DerivedTypeDef { components, .. } => {
            for component in components {
                collect_reference_decl(component, shadowed, facts);
            }
        }
        _ => {}
    }
}

fn collect_block_binding_names(decls: &[SpannedDecl], out: &mut HashSet<String>) {
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl { entities, .. } => {
                out.extend(entities.iter().map(|entity| entity.name.to_lowercase()));
            }
            Decl::ParameterStmt { pairs } => {
                out.extend(pairs.iter().map(|(name, _)| name.to_lowercase()));
            }
            _ => {}
        }
    }
}

fn collect_block_use_names(uses: &[SpannedDecl], out: &mut HashSet<String>) {
    for use_stmt in uses {
        let Decl::UseStmt { renames, only, .. } = &use_stmt.node else {
            continue;
        };
        out.extend(renames.iter().map(|rename| rename.local.to_lowercase()));
        if let Some(items) = only {
            for item in items {
                match item {
                    crate::ast::decl::OnlyItem::Name(name)
                    | crate::ast::decl::OnlyItem::Generic(name) => {
                        out.insert(name.to_lowercase());
                    }
                    crate::ast::decl::OnlyItem::Rename(rename) => {
                        out.insert(rename.local.to_lowercase());
                    }
                }
            }
        }
    }
}

fn collect_reference_stmt(
    stmt: &SpannedStmt,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    match &stmt.node {
        Stmt::Assignment { target, value } | Stmt::PointerAssignment { target, value } => {
            collect_reference_expr(target, shadowed, facts);
            collect_reference_expr(value, shadowed, facts);
        }
        Stmt::IfConstruct {
            condition,
            then_body,
            else_ifs,
            else_body,
            ..
        } => {
            collect_reference_expr(condition, shadowed, facts);
            collect_reference_stmts(then_body, shadowed, facts);
            for (condition, body) in else_ifs {
                collect_reference_expr(condition, shadowed, facts);
                collect_reference_stmts(body, shadowed, facts);
            }
            if let Some(body) = else_body {
                collect_reference_stmts(body, shadowed, facts);
            }
        }
        Stmt::IfStmt { condition, action } => {
            collect_reference_expr(condition, shadowed, facts);
            collect_reference_stmt(action, shadowed, facts);
        }
        Stmt::DoLoop {
            var,
            start,
            end,
            step,
            body,
            ..
        } => {
            if let Some(var) = var {
                let key = var.to_lowercase();
                if !shadowed.contains(&key) {
                    facts.references.push((key, stmt.span));
                }
            }
            for expr in [start, end, step].into_iter().flatten() {
                collect_reference_expr(expr, shadowed, facts);
            }
            collect_reference_stmts(body, shadowed, facts);
        }
        Stmt::DoWhile {
            condition, body, ..
        } => {
            collect_reference_expr(condition, shadowed, facts);
            collect_reference_stmts(body, shadowed, facts);
        }
        Stmt::DoConcurrent {
            controls,
            mask,
            locality,
            body,
            ..
        } => {
            for control in controls {
                collect_reference_expr(&control.start, shadowed, facts);
                collect_reference_expr(&control.end, shadowed, facts);
                if let Some(step) = &control.step {
                    collect_reference_expr(step, shadowed, facts);
                }
            }
            let mut nested_shadowed = shadowed.clone();
            nested_shadowed.extend(controls.iter().map(|control| control.var.to_lowercase()));
            for spec in locality {
                match spec {
                    LocalitySpec::Local(names) => {
                        nested_shadowed.extend(names.iter().map(|name| name.to_lowercase()));
                    }
                    LocalitySpec::LocalInit(names) | LocalitySpec::Reduce { vars: names, .. } => {
                        for name in names {
                            let key = name.to_lowercase();
                            if !shadowed.contains(&key) {
                                facts.references.push((key.clone(), stmt.span));
                            }
                            nested_shadowed.insert(key);
                        }
                    }
                    LocalitySpec::Shared(names) => {
                        for name in names {
                            let key = name.to_lowercase();
                            if !shadowed.contains(&key) {
                                facts.references.push((key, stmt.span));
                            }
                        }
                    }
                    LocalitySpec::DefaultNone => {}
                }
            }
            if let Some(mask) = mask {
                collect_reference_expr(mask, &nested_shadowed, facts);
            }
            collect_reference_stmts(body, &nested_shadowed, facts);
        }
        Stmt::SelectCase {
            selector, cases, ..
        } => {
            collect_reference_expr(selector, shadowed, facts);
            for case in cases {
                for selector in &case.selectors {
                    match selector {
                        CaseSelector::Value(expr) => {
                            collect_reference_expr(expr, shadowed, facts);
                        }
                        CaseSelector::Range { low, high } => {
                            for expr in [low, high].into_iter().flatten() {
                                collect_reference_expr(expr, shadowed, facts);
                            }
                        }
                        CaseSelector::Default => {}
                    }
                }
                collect_reference_stmts(&case.body, shadowed, facts);
            }
        }
        Stmt::SelectType {
            selector,
            assoc_name,
            guards,
            ..
        } => {
            collect_reference_expr(selector, shadowed, facts);
            let mut nested_shadowed = shadowed.clone();
            if let Some(name) = assoc_name {
                nested_shadowed.insert(name.to_lowercase());
            }
            for guard in guards {
                let body = match guard {
                    TypeGuard::TypeIs { body, .. }
                    | TypeGuard::ClassIs { body, .. }
                    | TypeGuard::ClassDefault { body } => body,
                };
                collect_reference_stmts(body, &nested_shadowed, facts);
            }
        }
        Stmt::SelectRank {
            selector,
            assoc_name,
            guards,
            ..
        } => {
            collect_reference_expr(selector, shadowed, facts);
            let mut nested_shadowed = shadowed.clone();
            if let Some(name) = assoc_name {
                nested_shadowed.insert(name.to_lowercase());
            }
            for guard in guards {
                let body = match guard {
                    RankGuard::Rank { body, .. }
                    | RankGuard::RankStar { body }
                    | RankGuard::RankDefault { body } => body,
                };
                collect_reference_stmts(body, &nested_shadowed, facts);
            }
        }
        Stmt::WhereConstruct {
            mask,
            body,
            elsewhere,
            ..
        } => {
            collect_reference_expr(mask, shadowed, facts);
            collect_reference_stmts(body, shadowed, facts);
            for (mask, body) in elsewhere {
                if let Some(mask) = mask {
                    collect_reference_expr(mask, shadowed, facts);
                }
                collect_reference_stmts(body, shadowed, facts);
            }
        }
        Stmt::WhereStmt { mask, stmt } => {
            collect_reference_expr(mask, shadowed, facts);
            collect_reference_stmt(stmt, shadowed, facts);
        }
        Stmt::ForallConstruct {
            specs, mask, body, ..
        } => {
            for spec in specs {
                collect_reference_expr(&spec.start, shadowed, facts);
                collect_reference_expr(&spec.end, shadowed, facts);
                if let Some(step) = &spec.step {
                    collect_reference_expr(step, shadowed, facts);
                }
            }
            let mut nested_shadowed = shadowed.clone();
            nested_shadowed.extend(specs.iter().map(|spec| spec.var.to_lowercase()));
            if let Some(mask) = mask {
                collect_reference_expr(mask, &nested_shadowed, facts);
            }
            collect_reference_stmts(body, &nested_shadowed, facts);
        }
        Stmt::ForallStmt { specs, mask, stmt } => {
            for spec in specs {
                collect_reference_expr(&spec.start, shadowed, facts);
                collect_reference_expr(&spec.end, shadowed, facts);
                if let Some(step) = &spec.step {
                    collect_reference_expr(step, shadowed, facts);
                }
            }
            let mut nested_shadowed = shadowed.clone();
            nested_shadowed.extend(specs.iter().map(|spec| spec.var.to_lowercase()));
            if let Some(mask) = mask {
                collect_reference_expr(mask, &nested_shadowed, facts);
            }
            collect_reference_stmt(stmt, &nested_shadowed, facts);
        }
        Stmt::Block {
            uses,
            implicit,
            decls,
            body,
            ..
        } => {
            let mut nested_shadowed = shadowed.clone();
            collect_block_use_names(uses, &mut nested_shadowed);
            collect_block_binding_names(decls, &mut nested_shadowed);
            for decl in uses.iter().chain(implicit).chain(decls) {
                collect_reference_decl(decl, &nested_shadowed, facts);
            }
            collect_reference_stmts(body, &nested_shadowed, facts);
        }
        Stmt::Associate { assocs, body, .. } => {
            for (_, expr) in assocs {
                collect_reference_expr(expr, shadowed, facts);
            }
            let mut nested_shadowed = shadowed.clone();
            nested_shadowed.extend(assocs.iter().map(|(name, _)| name.to_lowercase()));
            collect_reference_stmts(body, &nested_shadowed, facts);
        }
        Stmt::Stop { code, .. } | Stmt::ErrorStop { code, .. } | Stmt::Return { value: code } => {
            if let Some(code) = code {
                collect_reference_expr(code, shadowed, facts);
            }
        }
        Stmt::ComputedGoto { selector, .. } | Stmt::ArithmeticIf { expr: selector, .. } => {
            collect_reference_expr(selector, shadowed, facts);
        }
        Stmt::Labeled { stmt, .. } => collect_reference_stmt(stmt, shadowed, facts),
        Stmt::Write { controls, items }
        | Stmt::Read { controls, items }
        | Stmt::Inquire {
            specs: controls,
            items,
        } => {
            for control in controls {
                collect_reference_expr(&control.value, shadowed, facts);
            }
            for item in items {
                collect_reference_expr(item, shadowed, facts);
            }
        }
        Stmt::Open { specs }
        | Stmt::Close { specs }
        | Stmt::Rewind { specs }
        | Stmt::Backspace { specs }
        | Stmt::Endfile { specs }
        | Stmt::Flush { specs }
        | Stmt::Wait { specs } => {
            for spec in specs {
                collect_reference_expr(&spec.value, shadowed, facts);
            }
        }
        Stmt::Allocate {
            type_spec,
            items,
            opts,
        } => {
            if let Some(type_spec) = type_spec {
                collect_reference_type_spec(type_spec, shadowed, facts);
            }
            for item in items {
                collect_reference_expr(item, shadowed, facts);
            }
            for opt in opts {
                collect_reference_expr(&opt.value, shadowed, facts);
            }
        }
        Stmt::Deallocate { items, opts } => {
            for item in items {
                collect_reference_expr(item, shadowed, facts);
            }
            for opt in opts {
                collect_reference_expr(&opt.value, shadowed, facts);
            }
        }
        Stmt::Nullify { items } => {
            for item in items {
                collect_reference_expr(item, shadowed, facts);
            }
        }
        Stmt::Call { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                if !shadowed.contains(&key) {
                    facts.calls.insert(key);
                }
            }
            collect_reference_expr(callee, shadowed, facts);
            for arg in args {
                collect_reference_subscript(&arg.value, shadowed, facts);
            }
        }
        Stmt::Print { format, items } => {
            collect_reference_expr(format, shadowed, facts);
            for item in items {
                collect_reference_expr(item, shadowed, facts);
            }
        }
        Stmt::Namelist { groups } => {
            for (_, names) in groups {
                for name in names {
                    let key = name.to_lowercase();
                    if !shadowed.contains(&key) {
                        facts.references.push((key, stmt.span));
                    }
                }
            }
        }
        Stmt::Declaration(decl) => collect_reference_decl(decl, shadowed, facts),
        Stmt::Exit { .. }
        | Stmt::Cycle { .. }
        | Stmt::Goto { .. }
        | Stmt::Continue { .. }
        | Stmt::Format { .. } => {}
    }
}

fn collect_reference_stmts(
    stmts: &[SpannedStmt],
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    for stmt in stmts {
        collect_reference_stmt(stmt, shadowed, facts);
    }
}

fn procedure_reference_facts(unit: &ProgramUnit) -> ProcedureReferenceFacts {
    let (decls, body) = match unit {
        ProgramUnit::Program { decls, body, .. }
        | ProgramUnit::Subroutine { decls, body, .. }
        | ProgramUnit::Function { decls, body, .. } => (decls.as_slice(), body.as_slice()),
        _ => return ProcedureReferenceFacts::default(),
    };
    let shadowed = HashSet::new();
    let mut facts = ProcedureReferenceFacts::default();
    for decl in decls {
        collect_reference_decl(decl, &shadowed, &mut facts);
    }
    collect_reference_stmts(body, &shadowed, &mut facts);
    facts
}

fn contained_unit_name(unit: &ProgramUnit) -> Option<String> {
    match unit {
        ProgramUnit::Subroutine { name, .. } | ProgramUnit::Function { name, .. } => {
            Some(name.to_lowercase())
        }
        _ => None,
    }
}

fn procedure_host_scopes(st: &SymbolTable, owner_scope: ScopeId) -> HashSet<ScopeId> {
    let mut scopes = HashSet::new();
    let mut current = Some(owner_scope);
    while let Some(scope_id) = current {
        let scope = st.scope(scope_id);
        match scope.kind {
            ScopeKind::Program(_) | ScopeKind::Subroutine(_) | ScopeKind::Function(_) => {
                scopes.insert(scope_id);
            }
            ScopeKind::Global | ScopeKind::Module(_) | ScopeKind::Submodule(_) => break,
            _ => {}
        }
        current = scope.parent;
    }
    scopes
}

fn resolved_contained_calls(
    ctx: &Ctx<'_>,
    unit: &ProgramUnit,
    caller_scope: ScopeId,
    owner_scope: ScopeId,
    child_names: &HashSet<String>,
) -> HashSet<String> {
    procedure_reference_facts(unit)
        .calls
        .into_iter()
        .filter(|callee| {
            child_names.contains(callee)
                && ctx
                    .st
                    .lookup_in(caller_scope, callee)
                    .is_some_and(|symbol| {
                        symbol.scope == owner_scope
                            && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Subroutine)
                    })
        })
        .collect()
}

fn validate_finalizer_capture_references(ctx: &mut Ctx<'_>, unit: &ProgramUnit) {
    if ctx.finalizer_capture_host_scopes.is_empty() {
        return;
    }
    let facts = procedure_reference_facts(unit);
    for (name, span) in facts.references {
        let Some(symbol) = ctx.st.lookup_in(ctx.scope_id, &name) else {
            continue;
        };
        let Some(host_scope) = ctx
            .finalizer_capture_host_scopes
            .iter()
            .copied()
            .find(|host_scope| symbol.scope == *host_scope)
        else {
            continue;
        };
        let requires_host_storage = matches!(
            symbol.kind,
            SymbolKind::Variable | SymbolKind::ProcedurePointer
        ) || (matches!(symbol.kind, SymbolKind::Parameter)
            && !symbol.attrs.array_spec.is_empty());
        if !requires_host_storage {
            continue;
        }
        let key = (ctx.scope_id, host_scope, name.clone());
        if ctx.reported_finalizer_captures.insert(key) {
            ctx.error(
                span,
                format!(
                    "local FINAL procedure cannot reference host entity '{}': deferred finalization cannot preserve procedure host associations; move the state to module storage",
                    name
                ),
            );
        }
    }
}

fn validate_contained_units(ctx: &mut Ctx<'_>, host: &ProgramUnit, contains: &[SpannedUnit]) {
    if contains.is_empty() {
        return;
    }

    let child_names: HashSet<String> = contains
        .iter()
        .filter_map(|unit| contained_unit_name(&unit.node))
        .collect();
    let mut child_scopes: HashMap<String, HashSet<ScopeId>> = HashMap::new();

    if let Some(layouts) = ctx.type_layouts {
        for layout in layouts.iter_layouts().filter(|layout| {
            layout.owner_module.is_none() && layout.owner_scope == Some(ctx.scope_id)
        }) {
            for final_proc in &layout.final_procs {
                let name = final_proc.name.to_lowercase();
                if child_names.contains(&name) {
                    child_scopes
                        .entry(name)
                        .or_default()
                        .extend(procedure_host_scopes(ctx.st, ctx.scope_id));
                }
            }
        }
    }

    if !ctx.finalizer_capture_host_scopes.is_empty() {
        for callee in resolved_contained_calls(ctx, host, ctx.scope_id, ctx.scope_id, &child_names)
        {
            child_scopes
                .entry(callee)
                .or_default()
                .extend(ctx.finalizer_capture_host_scopes.iter().copied());
        }
    }

    let call_graph: HashMap<String, HashSet<String>> = contains
        .iter()
        .filter_map(|unit| {
            let name = contained_unit_name(&unit.node)?;
            let caller_scope = find_scope_for_unit(ctx.st, &unit.node, ctx.scope_id)?;
            let calls =
                resolved_contained_calls(ctx, &unit.node, caller_scope, ctx.scope_id, &child_names);
            Some((name, calls))
        })
        .collect();

    loop {
        let mut changed = false;
        for (caller, callees) in &call_graph {
            let Some(scopes) = child_scopes.get(caller).cloned() else {
                continue;
            };
            for callee in callees {
                let target = child_scopes.entry(callee.clone()).or_default();
                let before = target.len();
                target.extend(scopes.iter().copied());
                changed |= target.len() != before;
            }
        }
        if !changed {
            break;
        }
    }

    let inherited = std::mem::take(&mut ctx.finalizer_capture_host_scopes);
    for unit in contains {
        let name = contained_unit_name(&unit.node).unwrap_or_default();
        ctx.finalizer_capture_host_scopes = child_scopes.remove(&name).unwrap_or_default();
        validate_unit(ctx, unit);
    }
    ctx.finalizer_capture_host_scopes = inherited;
}

fn validate_unit(ctx: &mut Ctx, unit: &SpannedUnit) {
    let saved_scope = ctx.scope_id;
    if let Some(scope_id) = find_scope_for_unit(ctx.st, &unit.node, ctx.scope_id) {
        ctx.scope_id = scope_id;
    }
    validate_finalizer_capture_references(ctx, &unit.node);

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
                    ctx.require_std(implicit_stmt.span, FortranStandard::F90, "IMPLICIT NONE");
                }
            }
            if !contains.is_empty() {
                ctx.require_std(
                    unit.span,
                    FortranStandard::F90,
                    "CONTAINS/internal procedures",
                );
            }
            validate_decls(ctx, decls);
            check_implicit_none(ctx, body, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            validate_contained_units(ctx, &unit.node, contains);
        }
        ProgramUnit::Module {
            uses,
            implicit,
            decls,
            contains,
            ..
        } => {
            ctx.require_std(unit.span, FortranStandard::F90, "MODULE");
            for use_stmt in uses {
                ctx.require_std(use_stmt.span, FortranStandard::F90, "USE statement");
            }
            for implicit_stmt in implicit {
                if matches!(implicit_stmt.node, Decl::ImplicitNone { .. }) {
                    ctx.require_std(implicit_stmt.span, FortranStandard::F90, "IMPLICIT NONE");
                }
            }
            validate_decls(ctx, decls);
            validate_contained_units(ctx, &unit.node, contains);
        }
        ProgramUnit::Subroutine {
            name,
            prefix,
            uses,
            implicit,
            decls,
            body,
            contains,
            args,
            bind,
            ..
        } => {
            validate_smp_body(ctx, name, prefix, unit.span);
            let saved_pure = ctx.in_pure;
            let saved_elemental = ctx.in_elemental;
            let saved_bind_c = ctx.in_bind_c_unit;
            ctx.in_bind_c_unit = bind.is_some();
            ctx.in_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            ctx.in_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            let is_impure = prefix.iter().any(|p| matches!(p, Prefix::Impure));
            // F2008 §12.6.2.2: ELEMENTAL implies PURE unless IMPURE
            // is also given.  `impure elemental function f(...)` is
            // explicitly allowed to call non-pure callees.
            if ctx.in_elemental && !is_impure {
                ctx.in_pure = true;
            }
            if is_impure {
                ctx.in_pure = false;
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
                    ctx.require_std(implicit_stmt.span, FortranStandard::F90, "IMPLICIT NONE");
                }
            }
            if !contains.is_empty() {
                ctx.require_std(
                    unit.span,
                    FortranStandard::F90,
                    "CONTAINS/internal procedures",
                );
            }
            let saved_args = std::mem::replace(
                &mut ctx.current_args,
                args.iter()
                    .filter_map(|a| match a {
                        crate::ast::unit::DummyArg::Name(n) => Some(n.to_lowercase()),
                        crate::ast::unit::DummyArg::Star => None,
                    })
                    .collect(),
            );
            validate_decls(ctx, decls);
            check_implicit_none(ctx, body, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            validate_contained_units(ctx, &unit.node, contains);
            ctx.current_args = saved_args;
            ctx.in_pure = saved_pure;
            ctx.in_elemental = saved_elemental;
            ctx.in_bind_c_unit = saved_bind_c;
        }
        ProgramUnit::Function {
            name,
            prefix,
            uses,
            implicit,
            decls,
            body,
            contains,
            args,
            bind,
            result,
            return_type,
            ..
        } => {
            validate_smp_body(ctx, name, prefix, unit.span);
            let saved_pure = ctx.in_pure;
            let saved_elemental = ctx.in_elemental;
            let saved_bind_c = ctx.in_bind_c_unit;
            ctx.in_bind_c_unit = bind.is_some();
            // A BIND(C) function returning a derived type hits the
            // unimplemented aggregate-return ABI: the result comes back
            // from a never-written buffer (reads 0), broken at every
            // struct size and on every target. Reject loudly until the C
            // struct-return convention lands. Audit finding C2.
            if bind.is_some() {
                let result_name = result
                    .clone()
                    .unwrap_or_else(|| name.clone())
                    .to_lowercase();
                let result_ts = return_type.clone().or_else(|| {
                    decls.iter().find_map(|d| match &d.node {
                        Decl::TypeDecl {
                            type_spec,
                            entities,
                            ..
                        } if entities
                            .iter()
                            .any(|e| e.name.eq_ignore_ascii_case(&result_name)) =>
                        {
                            Some(type_spec.clone())
                        }
                        _ => None,
                    })
                });
                if matches!(
                    result_ts,
                    Some(crate::ast::decl::TypeSpec::Type(_))
                        | Some(crate::ast::decl::TypeSpec::Class(_))
                ) && !result_ts
                    .as_ref()
                    .is_some_and(is_c_interop_pointer_typespec)
                {
                    ctx.error(
                        unit.span,
                        "a BIND(C) function returning a derived type is not supported yet \
                         (aggregate return ABI is a separate calling-convention change)",
                    );
                }
            }
            // A prefix return type of real(16)/complex(16) — or any
            // real/complex kind outside {4, 8} — silently computes the
            // result in single precision (no backend float wider than 64
            // bits). The `result(r)` body-declaration spelling is caught by
            // validate_decls; the prefix `real(16) function f()` spelling is
            // checked here. Audit finding C7.
            if let Some(TypeSpec::Real(sel) | TypeSpec::Complex(sel)) = &return_type {
                if let Some(k) = eval_real_complex_kind(ctx, sel) {
                    if k != 4 && k != 8 {
                        let what = if matches!(return_type, Some(TypeSpec::Complex(_))) {
                            "COMPLEX"
                        } else {
                            "REAL"
                        };
                        ctx.error(
                            unit.span,
                            format!(
                                "{what}(kind={k}) function result is not supported: the backend \
                                 has no float wider than 64 bits, so only kind 4 and kind 8 exist. \
                                 (Previously this was silently computed in single precision.)"
                            ),
                        );
                    }
                }
            }
            ctx.in_pure = prefix.iter().any(|p| matches!(p, Prefix::Pure));
            ctx.in_elemental = prefix.iter().any(|p| matches!(p, Prefix::Elemental));
            let is_impure = prefix.iter().any(|p| matches!(p, Prefix::Impure));
            // F2008 §12.6.2.2: see comment above for Subroutine.
            if ctx.in_elemental && !is_impure {
                ctx.in_pure = true;
            }
            if is_impure {
                ctx.in_pure = false;
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
                    ctx.require_std(implicit_stmt.span, FortranStandard::F90, "IMPLICIT NONE");
                }
            }
            if !contains.is_empty() {
                ctx.require_std(
                    unit.span,
                    FortranStandard::F90,
                    "CONTAINS/internal procedures",
                );
            }
            let saved_args = std::mem::replace(
                &mut ctx.current_args,
                args.iter()
                    .filter_map(|a| match a {
                        crate::ast::unit::DummyArg::Name(n) => Some(n.to_lowercase()),
                        crate::ast::unit::DummyArg::Star => None,
                    })
                    .collect(),
            );
            validate_decls(ctx, decls);
            check_implicit_none(ctx, body, decls);
            validate_stmts(ctx, body);
            validate_label_consistency(ctx, unit.span);
            validate_contained_units(ctx, &unit.node, contains);
            ctx.current_args = saved_args;
            ctx.in_pure = saved_pure;
            ctx.in_elemental = saved_elemental;
            ctx.in_bind_c_unit = saved_bind_c;
        }
        ProgramUnit::Submodule {
            parent,
            uses,
            decls,
            contains,
            ..
        } => {
            ctx.require_std(unit.span, FortranStandard::F2008, "SUBMODULE");
            // F2008 C1113: the parent (ancestor module or parent
            // submodule) must be available. If neither a module nor a
            // submodule of that name is in scope (in-file or loaded from
            // an .amod), the submodule can't inherit anything — diagnose
            // it instead of silently producing a dangling unit.
            let parent_exists = ctx.st.find_module_scope(parent).is_some()
                || ctx.st.all_scopes().iter().any(|s| {
                    matches!(&s.kind, ScopeKind::Submodule(n) if n.eq_ignore_ascii_case(parent))
                });
            if !parent_exists {
                ctx.error(
                    unit.span,
                    format!(
                        "SUBMODULE parent '{parent}' not found — no such module or \
                         submodule is available (compile it first or provide its .amod)"
                    ),
                );
            }
            for use_stmt in uses {
                ctx.require_std(use_stmt.span, FortranStandard::F90, "USE statement");
            }
            validate_decls(ctx, decls);
            validate_contained_units(ctx, &unit.node, contains);
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
        validate_decl_const_int_exprs(ctx, decl);

        if let Decl::TypeDecl {
            attrs,
            entities,
            type_spec,
            ..
        } = &decl.node
        {
            let has_alloc = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
            let has_pointer = attrs.iter().any(|a| matches!(a, Attribute::Pointer));

            // RANK(n) (F2023 8.5.17). The parser already desugared the
            // shape to a deferred-shape Dimension; here the marker
            // drives conformance and the C-constraint: a rank>0 entity
            // must be a dummy argument or carry ALLOCATABLE/POINTER.
            if let Some(n) = attrs.iter().find_map(|a| match a {
                Attribute::Rank(n) => Some(*n),
                _ => None,
            }) {
                ctx.require_std(decl.span, FortranStandard::F2023, "the RANK(n) attribute");
                if n > 0 && !has_alloc && !has_pointer {
                    for entity in entities {
                        if !ctx.current_args.contains(&entity.name.to_lowercase()) {
                            ctx.error(
                                decl.span,
                                format!(
                                    "RANK({}) entity '{}' must be a dummy argument or have \
                                     ALLOCATABLE or POINTER",
                                    n, entity.name
                                ),
                            );
                        }
                    }
                }
            }
            let is_scalar_decl = entities.iter().all(|entity| entity.array_spec.is_none());
            let has_dimension_attr = attrs.iter().any(|a| matches!(a, Attribute::Dimension(_)));

            if has_alloc || has_pointer {
                for entity in entities {
                    if entity.array_spec.is_some() || has_dimension_attr {
                        ctx.allocatable_array_targets
                            .insert((ctx.scope_id, entity.name.to_lowercase()));
                    }
                }
            }

            // Character VALUE dummies are not lowered with copy-in
            // semantics yet: the callee receives the caller's storage
            // pointer, so mutation corrupts the caller (or SEGVs on a
            // literal actual). Correct lowering is a dedicated
            // calling-convention effort (see noted_items.md, "CHARACTER
            // VALUE copy-in"). BIND(C) procedures are exempt: their
            // c_char VALUE dummies take the working byte-copy path.
            if attrs.iter().any(|a| matches!(a, Attribute::Value))
                && matches!(type_spec, crate::ast::decl::TypeSpec::Character(_))
                && !ctx.in_bind_c_unit
            {
                ctx.error(
                    decl.span,
                    "the VALUE attribute on CHARACTER dummies is not supported yet \
                     (copy-in lowering is a separate calling-convention change)",
                );
            }

            // Derived-type / CLASS VALUE dummies are not lowered: the
            // callee reads the dummy's components as constant 0 instead
            // of the passed aggregate (the SysV/AAPCS64 by-value struct
            // ABI is unwired — the classifier in src/codegen/x86/abi.rs
            // has no producer). This is a silent miscompile in both
            // directions on every target (the by-pointer IR is
            // target-independent), so reject loudly until the aggregate
            // by-value calling convention lands. Audit finding C2.
            if attrs.iter().any(|a| matches!(a, Attribute::Value))
                && matches!(
                    type_spec,
                    crate::ast::decl::TypeSpec::Type(_) | crate::ast::decl::TypeSpec::Class(_)
                )
                && !is_c_interop_pointer_typespec(type_spec)
            {
                ctx.error(
                    decl.span,
                    "the VALUE attribute on derived-type dummies is not supported yet \
                     (by-value aggregate passing is a separate calling-convention change)",
                );
            }

            // real(16)/complex(16) (IEEE quad) — and any other real/complex
            // kind outside {4, 8} — were silently downgraded to single
            // precision: the IR maps kind 8 -> f64 and every other kind ->
            // f32 (src/ir/types.rs float_from_kind), so kind() reported 4 and
            // the value was computed in single precision; a complex(16)
            // result additionally mis-sized its buffer and SIGSEGV'd at exit.
            // The backend has no float wider than 64 bits, so reject the
            // unsupported kind loudly instead of miscompiling. Audit finding
            // C7. Only reject when the kind evaluates to a definite value —
            // an unresolved kind selector is left alone.
            if let TypeSpec::Real(sel) | TypeSpec::Complex(sel) = type_spec {
                if let Some(k) = eval_real_complex_kind(ctx, sel) {
                    if k != 4 && k != 8 {
                        let what = if matches!(type_spec, TypeSpec::Complex(_)) {
                            "COMPLEX"
                        } else {
                            "REAL"
                        };
                        ctx.error(
                            decl.span,
                            format!(
                                "{what}(kind={k}) is not supported: the backend has no float \
                                 wider than 64 bits, so only kind 4 and kind 8 exist. \
                                 (Previously this was silently computed in single precision.)"
                            ),
                        );
                    }
                }
            }

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
                TypeSpec::TypeOf(entity) | TypeSpec::ClassOf(entity) => {
                    let is_classof = matches!(type_spec, TypeSpec::ClassOf(_));
                    ctx.require_std(
                        decl.span,
                        FortranStandard::F2023,
                        if is_classof { "CLASSOF" } else { "TYPEOF" },
                    );
                    // The entity must be previously declared with a
                    // complete type (resolution is source-ordered, so
                    // a forward reference fails this lookup too).
                    match ctx.st.find_symbol_any_scope(&entity.to_lowercase()) {
                        None => ctx.error(
                            decl.span,
                            format!(
                                "{}({}) references an undeclared entity",
                                if is_classof { "CLASSOF" } else { "TYPEOF" },
                                entity
                            ),
                        ),
                        Some(sym) => match &sym.type_info {
                            None => ctx.error(
                                decl.span,
                                format!(
                                    "{}({}) requires an entity with a complete type",
                                    if is_classof { "CLASSOF" } else { "TYPEOF" },
                                    entity
                                ),
                            ),
                            Some(crate::sema::symtab::TypeInfo::ClassStar)
                            | Some(crate::sema::symtab::TypeInfo::TypeStar) => ctx.error(
                                decl.span,
                                format!(
                                    "{}({}) of an unlimited polymorphic or assumed-type \
                                     entity is not allowed",
                                    if is_classof { "CLASSOF" } else { "TYPEOF" },
                                    entity
                                ),
                            ),
                            Some(ti)
                                if is_classof
                                    && !matches!(
                                        ti,
                                        crate::sema::symtab::TypeInfo::Derived(_)
                                            | crate::sema::symtab::TypeInfo::Class(_)
                                    ) =>
                            {
                                ctx.error(
                                    decl.span,
                                    format!(
                                        "CLASSOF({}) requires an entity of derived or \
                                         CLASS type",
                                        entity
                                    ),
                                );
                            }
                            _ => {}
                        },
                    }
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

        if let Decl::EnumDef { type_name, .. } = &decl.node {
            ctx.require_std(decl.span, FortranStandard::F2003, "ENUM, BIND(C)");
            if type_name.is_some() {
                ctx.require_std(
                    decl.span,
                    FortranStandard::F2023,
                    "named interoperable ENUM type",
                );
            }
        }

        if matches!(decl.node, Decl::EnumerationTypeDef { .. }) {
            ctx.require_std(decl.span, FortranStandard::F2023, "ENUMERATION TYPE");
        }

        if let Decl::CommonBlock { .. } = &decl.node {
            warn_legacy_feature(ctx, decl.span, "COMMON block");
            // Character members now lower with inline-byte storage
            // (fixed-length); storage association works. See l06.
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
            validate_unsupported_component_forms(ctx, components);
            validate_derived_type(
                ctx,
                name,
                type_attrs,
                type_bound_procs,
                components,
                decl.span,
            );
            let has_inline_cycle = ctx.type_layouts.is_some_and(|layouts| {
                layouts
                    .get_for_scope(ctx.scope_id, name)
                    .or_else(|| layouts.get(name))
                    .is_some_and(|layout| layouts.has_inline_storage_cycle(layout))
            });
            if has_inline_cycle {
                ctx.error(
                    decl.span,
                    format!(
                        "derived type '{}' has a recursive component that requires infinite inline storage; make the recursive component POINTER or ALLOCATABLE",
                        name
                    ),
                );
            } else if ctx
                .type_layouts
                .and_then(|layouts| {
                    let layout = layouts
                        .get_for_scope(ctx.scope_id, name)
                        .or_else(|| layouts.get(name))?;
                    layouts
                        .deallocation_has_ownerless_finalizer(layout)
                        .then_some(())
                })
                .is_some()
            {
                ctx.error(
                    decl.span,
                    format!(
                        "locally declared derived type '{}' combines allocatable ownership with a local FINAL binding that generated cleanup cannot call; move the type declaration and its FINAL procedures to module scope",
                        name
                    ),
                );
            }
        }
    }
}

// ---- Statement validation ----

fn validate_stmts(ctx: &mut Ctx, stmts: &[SpannedStmt]) {
    for stmt in stmts {
        validate_stmt(ctx, stmt);
    }
}

#[derive(Default)]
struct AllocationOptionPresence {
    stat: bool,
    errmsg: bool,
    source: bool,
    mold: bool,
}

fn validate_allocation_options(
    ctx: &mut Ctx<'_>,
    stmt_span: Span,
    statement: &str,
    opts: &[IoControl],
    type_spec_present: bool,
) -> AllocationOptionPresence {
    let mut presence = AllocationOptionPresence::default();

    for opt in opts {
        let Some(keyword) = opt.keyword.as_deref() else {
            ctx.error(
                opt.value.span,
                format!("{statement} options require keyword syntax"),
            );
            continue;
        };
        let keyword = keyword.to_ascii_lowercase();
        let seen = match keyword.as_str() {
            "stat" => &mut presence.stat,
            "errmsg" => &mut presence.errmsg,
            "source" => &mut presence.source,
            "mold" => &mut presence.mold,
            _ => {
                ctx.error(
                    opt.value.span,
                    format!(
                        "{statement} does not permit {}=",
                        keyword.to_ascii_uppercase()
                    ),
                );
                continue;
            }
        };
        let duplicate = *seen;
        *seen = true;
        if duplicate {
            ctx.error(
                opt.value.span,
                format!(
                    "{statement} cannot specify {}= more than once",
                    keyword.to_ascii_uppercase()
                ),
            );
            continue;
        }
        if statement == "DEALLOCATE" && matches!(keyword.as_str(), "source" | "mold") {
            ctx.error(
                opt.value.span,
                format!(
                    "DEALLOCATE does not permit {}=",
                    keyword.to_ascii_uppercase()
                ),
            );
        }
    }

    if presence.errmsg && !presence.stat {
        ctx.warning(stmt_span, "ERRMSG= has no effect without STAT=");
    }
    if type_spec_present && presence.source {
        ctx.error(
            stmt_span,
            "ALLOCATE type-spec cannot be combined with SOURCE=",
        );
    }
    if type_spec_present && presence.mold {
        ctx.error(
            stmt_span,
            "ALLOCATE type-spec cannot be combined with MOLD=",
        );
    }

    presence
}

fn validate_stmt(ctx: &mut Ctx, stmt: &SpannedStmt) {
    validate_stmt_const_int_exprs(ctx, stmt);
    validate_stmt_enum_usage(ctx, stmt);

    match &stmt.node {
        // ---- Assignment ----
        Stmt::Assignment { target, value } => {
            validate_assignment_target(ctx, target, stmt.span);
            reject_pure_nonlocal_definition(ctx, target, stmt.span, "assignment");
            if ctx.in_pure {
                check_pure_expr_calls(ctx, value);
            }
            if polymorphic_allocatable_target(ctx, target)
                && !assignment_uses_defined_assignment(ctx, target, value)
            {
                let unsupported_type = unsupported_polymorphic_ownership_from_expr(ctx, value);
                if let Some(type_name) = unsupported_type {
                    reject_context_dependent_polymorphic_ownership(ctx, value.span, &type_name);
                }
            }
        }
        Stmt::PointerAssignment { target, value, .. } => {
            // F2023 10.2.2.2 bounds remapping from an array constructor
            // (`q([2, 3]) => t`) lowers via remap_bounds_args.
            validate_pointer_assignment(ctx, target, value, stmt.span);
            reject_pure_nonlocal_definition(ctx, target, stmt.span, "pointer assignment");
        }

        // ---- Allocate / Deallocate ----
        Stmt::Allocate {
            type_spec,
            items,
            opts,
        } => {
            let option_presence =
                validate_allocation_options(ctx, stmt.span, "ALLOCATE", opts, type_spec.is_some());
            let has_source = option_presence.source;
            let has_mold = option_presence.mold;
            if has_source {
                ctx.require_std(stmt.span, FortranStandard::F2003, "ALLOCATE with SOURCE=");
            }
            if has_mold {
                ctx.require_std(stmt.span, FortranStandard::F2003, "ALLOCATE with MOLD=");
            }
            if has_source && has_mold {
                ctx.error(stmt.span, "ALLOCATE cannot specify both SOURCE= and MOLD=");
            }
            for item in items {
                validate_allocatable_item(ctx, item, "allocate");
                if !has_source && !has_mold && allocate_item_needs_explicit_shape(ctx, item) {
                    ctx.error(item.span, "array ALLOCATE requires bounds or SOURCE=/MOLD=");
                }
                if polymorphic_allocate_target(ctx, item) {
                    let unsupported_type =
                        unsupported_allocate_dynamic_type(ctx, type_spec.as_ref(), opts, item);
                    if let Some(type_name) = unsupported_type {
                        reject_context_dependent_polymorphic_ownership(ctx, item.span, &type_name);
                    }
                }
                // F2023 R936-R937: one array constructor may supply all
                // bounds (`allocate(x([2, 3]))`); lowered via
                // lower_alloc_bounds_list.
            }
        }
        Stmt::Deallocate { items, opts } => {
            validate_allocation_options(ctx, stmt.span, "DEALLOCATE", opts, false);
            for item in items {
                validate_allocatable_item(ctx, item, "deallocate");
            }
        }

        // ---- I/O in pure ----
        Stmt::Write { controls, .. } | Stmt::Read { controls, .. }
            if ctx.in_pure && !uses_internal_character_file(ctx, controls) =>
        {
            ctx.error(stmt.span, "I/O statement not allowed in pure procedure");
        }
        Stmt::Print { .. }
        | Stmt::Open { .. }
        | Stmt::Close { .. }
        | Stmt::Inquire { .. }
        | Stmt::Rewind { .. }
        | Stmt::Backspace { .. }
        | Stmt::Endfile { .. }
        | Stmt::Flush { .. }
        | Stmt::Wait { .. }
            if ctx.in_pure =>
        {
            ctx.error(stmt.span, "I/O statement not allowed in pure procedure");
        }

        // ---- STOP / ERROR STOP in pure ----
        // F2018 §11.4 forbids STOP in pure procedures; F2023 §11.4 explicitly
        // permits ERROR STOP in pure procedures, which stdlib relies on.
        Stmt::Stop { .. } if ctx.in_pure => {
            ctx.error(stmt.span, "STOP not allowed in pure procedure");
        }
        Stmt::ErrorStop { .. } => {
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
        Stmt::IfConstruct {
            then_body,
            else_ifs,
            else_body,
            ..
        } => {
            validate_stmts(ctx, then_body);
            for (_, body) in else_ifs {
                validate_stmts(ctx, body);
            }
            if let Some(body) = else_body {
                validate_stmts(ctx, body);
            }
        }
        Stmt::IfStmt { action, .. } => validate_stmt(ctx, action),
        Stmt::DoLoop {
            body,
            shared_terminating_label,
            ..
        } => {
            if *shared_terminating_label && (ctx.warn_pedantic || ctx.warn_deprecated) {
                ctx.warning(
                    stmt.span,
                    "shared DO termination label is a deleted feature",
                );
            }
            validate_stmts(ctx, body);
        }
        Stmt::DoWhile { body, .. } => validate_stmts(ctx, body),
        Stmt::DoConcurrent { body, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F2008, "DO CONCURRENT");
            validate_stmts(ctx, body);
        }
        Stmt::SelectCase { cases, .. } => {
            validate_select_case_arms(ctx, stmt.span, cases);
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
        Stmt::Block {
            uses,
            ifaces,
            implicit,
            decls,
            body,
            ..
        } => {
            ctx.require_std(stmt.span, FortranStandard::F2008, "BLOCK construct");
            validate_decls(ctx, uses);
            validate_decls(ctx, implicit);
            validate_decls(ctx, decls);
            for iface in ifaces {
                validate_unit(ctx, iface);
            }
            let frame = block_binding_frame(decls);
            ctx.block_decl_frames.push(frame);
            validate_stmts(ctx, body);
            ctx.block_decl_frames.pop();
        }
        Stmt::Associate { assocs, body, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F2003, "ASSOCIATE construct");
            validate_associate(ctx, assocs, body, stmt.span);
        }

        // Call in pure: callee must be pure (we check if it's known impure).
        Stmt::Call { callee, args, .. } => {
            if let Expr::Name { name } = &callee.node {
                let intrinsic_move_alloc = name.eq_ignore_ascii_case("move_alloc")
                    && ctx.lookup(name).is_none_or(|symbol| {
                        symbol.attrs.intrinsic || matches!(symbol.kind, SymbolKind::IntrinsicProc)
                    });
                if intrinsic_move_alloc {
                    ctx.require_std(stmt.span, FortranStandard::F2003, "MOVE_ALLOC");
                    validate_move_alloc_polymorphic_ownership(ctx, args);
                }
                if name.eq_ignore_ascii_case("system_clock") && ctx.lookup(name).is_none() {
                    validate_system_clock_args(ctx, args, stmt.span);
                }
            }
            if let Some(name) = call_target_function_name(ctx, callee) {
                ctx.error(
                    callee.span,
                    format!(
                        "function '{}' cannot be invoked with CALL; reference it in an expression",
                        name
                    ),
                );
            }
            if ctx.in_pure {
                validate_pure_call(ctx, callee, stmt.span);
            }
            validate_call_site_intent(ctx, callee, args, stmt.span);
        }

        // Nullify: items must be pointers.
        Stmt::Nullify { items } => {
            for item in items {
                if expr_selects_component(item) {
                    if let Some(leaf) = leaf_field_layout(ctx, item) {
                        if !leaf.field.pointer {
                            ctx.error(
                                item.span,
                                format!(
                                    "NULLIFY target component '{}' must have pointer attribute",
                                    leaf.field.name
                                ),
                            );
                        }
                    }
                } else if let Some(ref name) = extract_base_name(item) {
                    let is_pointer = ctx.lookup(name).map(|s| s.attrs.pointer).unwrap_or(true);
                    if !is_pointer {
                        ctx.error(
                            item.span,
                            format!("NULLIFY target '{}' must have pointer attribute", name),
                        );
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

#[derive(Debug, Clone, Copy)]
struct ConstCaseInterval {
    low: i128,
    high: i128,
}

fn case_selector_span(selector: &CaseSelector, fallback: Span) -> Span {
    match selector {
        CaseSelector::Value(expr) => expr.span,
        CaseSelector::Range { low, high } => low
            .as_ref()
            .map(|expr| expr.span)
            .or_else(|| high.as_ref().map(|expr| expr.span))
            .unwrap_or(fallback),
        CaseSelector::Default => fallback,
    }
}

fn eval_const_case_bound(
    ctx: &mut Ctx<'_>,
    expr: Option<&crate::ast::expr::SpannedExpr>,
) -> Option<i128> {
    let expr = expr?;
    match eval_const_int_expr_checked(ctx, expr) {
        Ok(Some(value)) => Some(value.value),
        Ok(None) => None,
        Err(diag) => {
            ctx.error(diag.span, diag.msg);
            None
        }
    }
}

fn const_case_interval(
    ctx: &mut Ctx<'_>,
    selector: &CaseSelector,
    stmt_span: Span,
) -> Option<ConstCaseInterval> {
    match selector {
        CaseSelector::Value(expr) => match eval_const_int_expr_checked(ctx, expr) {
            Ok(Some(value)) => Some(ConstCaseInterval {
                low: value.value,
                high: value.value,
            }),
            Ok(None) => None,
            Err(diag) => {
                ctx.error(diag.span, diag.msg);
                None
            }
        },
        CaseSelector::Range { low, high } => {
            let low_value = match low {
                Some(_) => eval_const_case_bound(ctx, low.as_ref())?,
                None => i128::MIN,
            };
            let high_value = match high {
                Some(_) => eval_const_case_bound(ctx, high.as_ref())?,
                None => i128::MAX,
            };
            if low_value > high_value {
                ctx.error(
                    case_selector_span(selector, stmt_span),
                    "SELECT CASE range lower bound exceeds upper bound",
                );
                return None;
            }
            Some(ConstCaseInterval {
                low: low_value,
                high: high_value,
            })
        }
        CaseSelector::Default => None,
    }
}

fn validate_select_case_arms(ctx: &mut Ctx<'_>, stmt_span: Span, cases: &[CaseBlock]) {
    let mut default_seen = false;
    let mut seen_intervals: Vec<ConstCaseInterval> = Vec::new();

    for case in cases {
        for selector in &case.selectors {
            if matches!(selector, CaseSelector::Default) {
                if default_seen {
                    ctx.error(
                        case_selector_span(selector, stmt_span),
                        "SELECT CASE cannot contain multiple CASE DEFAULT arms",
                    );
                } else {
                    default_seen = true;
                }
                continue;
            }

            let Some(interval) = const_case_interval(ctx, selector, stmt_span) else {
                continue;
            };
            if seen_intervals
                .iter()
                .any(|previous| interval.low <= previous.high && previous.low <= interval.high)
            {
                ctx.error(
                    case_selector_span(selector, stmt_span),
                    "SELECT CASE selectors must be mutually exclusive",
                );
                continue;
            }
            seen_intervals.push(interval);
        }
    }
}

// ---- Specific validation checks ----

fn derived_type_name_from_type_info(info: &TypeInfo) -> Option<String> {
    match info {
        TypeInfo::Derived(name) | TypeInfo::Class(name) => Some(name.clone()),
        _ => None,
    }
}

fn derived_type_name_for_expr(
    ctx: &Ctx<'_>,
    expr: &crate::ast::expr::SpannedExpr,
) -> Option<String> {
    match &expr.node {
        Expr::Name { name } => ctx
            .lookup(name)
            .and_then(|sym| sym.type_info.as_ref())
            .and_then(derived_type_name_from_type_info),
        Expr::ParenExpr { inner } => derived_type_name_for_expr(ctx, inner),
        Expr::FunctionCall { callee, .. } => {
            let Expr::Name { name } = &callee.node else {
                return derived_type_name_for_expr(ctx, callee);
            };
            let symbol = ctx.lookup(name)?;
            if matches!(symbol.kind, SymbolKind::DerivedType) {
                Some(symbol.name.clone())
            } else {
                symbol
                    .type_info
                    .as_ref()
                    .and_then(derived_type_name_from_type_info)
            }
        }
        Expr::ComponentAccess { .. } => resolve_component_access_type(ctx, expr).ok().flatten(),
        _ => None,
    }
}

fn fortran_type_to_validation_type_info(
    type_: crate::sema::types::FortranType,
) -> Option<TypeInfo> {
    use crate::sema::types::{CharLen, FortranType};
    match type_ {
        FortranType::Integer { kind } => Some(TypeInfo::Integer { kind: Some(kind) }),
        FortranType::Real { kind } => Some(TypeInfo::Real { kind: Some(kind) }),
        FortranType::Complex { kind } => Some(TypeInfo::Complex { kind: Some(kind) }),
        FortranType::Logical { kind } => Some(TypeInfo::Logical { kind: Some(kind) }),
        FortranType::Character { kind, len } => Some(TypeInfo::Character {
            len: match len {
                CharLen::Known(len) => Some(len),
                CharLen::Assumed | CharLen::Deferred | CharLen::Unknown => None,
            },
            kind: Some(kind),
        }),
        FortranType::Derived { name } => Some(TypeInfo::Derived(name)),
        FortranType::Enumeration { name } => Some(TypeInfo::Enumeration(name)),
        FortranType::ClassOf { base } => Some(TypeInfo::Class(base)),
        FortranType::UnlimitedPoly => Some(TypeInfo::ClassStar),
        FortranType::AssumedType => Some(TypeInfo::TypeStar),
        FortranType::Void | FortranType::Unknown => None,
    }
}

fn validation_expr_type_info(ctx: &Ctx<'_>, expr: &SpannedExpr) -> Option<TypeInfo> {
    if expr_selects_component(expr) {
        if let Some(leaf) = leaf_field_layout(ctx, expr) {
            return Some(leaf.field.type_info.clone());
        }
    }
    let resolved = match &expr.node {
        Expr::Name { name } => ctx.lookup(name).and_then(|symbol| symbol.type_info.clone()),
        Expr::ParenExpr { inner } => validation_expr_type_info(ctx, inner),
        Expr::FunctionCall { callee, .. } => {
            let Expr::Name { name } = &callee.node else {
                return validation_expr_type_info(ctx, callee);
            };
            ctx.lookup(name).and_then(|symbol| {
                if matches!(symbol.kind, SymbolKind::DerivedType) {
                    Some(TypeInfo::Derived(symbol.name.clone()))
                } else {
                    symbol.type_info.clone()
                }
            })
        }
        _ => None,
    };
    resolved.or_else(|| fortran_type_to_validation_type_info(expr_type(expr, ctx.st)))
}

fn polymorphic_allocatable_target(ctx: &Ctx<'_>, expr: &crate::ast::expr::SpannedExpr) -> bool {
    if expr_selects_component(expr) {
        return leaf_field_layout(ctx, expr).is_some_and(|leaf| {
            leaf.field.allocatable
                && matches!(
                    &leaf.field.type_info,
                    TypeInfo::Class(_) | TypeInfo::ClassStar
                )
        });
    }
    extract_base_name(expr)
        .and_then(|name| ctx.lookup(&name))
        .is_some_and(|symbol| {
            symbol.attrs.allocatable
                && matches!(
                    symbol.type_info.as_ref(),
                    Some(TypeInfo::Class(_) | TypeInfo::ClassStar)
                )
        })
}

fn polymorphic_allocate_target(ctx: &Ctx<'_>, expr: &SpannedExpr) -> bool {
    if expr_selects_component(expr) {
        return leaf_field_layout(ctx, expr).is_some_and(|leaf| {
            (leaf.field.allocatable || leaf.field.pointer)
                && matches!(
                    &leaf.field.type_info,
                    TypeInfo::Class(_) | TypeInfo::ClassStar
                )
        });
    }
    extract_base_name(expr)
        .and_then(|name| ctx.lookup(&name))
        .is_some_and(|symbol| {
            (symbol.attrs.allocatable || symbol.attrs.pointer)
                && matches!(
                    symbol.type_info.as_ref(),
                    Some(TypeInfo::Class(_) | TypeInfo::ClassStar)
                )
        })
}

fn ownerless_finalizer_type_name(ctx: &Ctx<'_>, type_name: &str) -> Option<String> {
    let layouts = ctx.type_layouts?;
    let layout = layouts
        .get_for_scope(ctx.scope_id, type_name)
        .or_else(|| layouts.get(type_name))?;
    layouts
        .lifecycle_has_ownerless_finalizer(layout)
        .then(|| layout.name.clone())
}

fn unsupported_polymorphic_ownership_from_expr(
    ctx: &Ctx<'_>,
    expr: &SpannedExpr,
) -> Option<String> {
    if let Some(type_name) = derived_type_name_for_expr(ctx, expr) {
        if let Some(unsupported) = ownerless_finalizer_type_name(ctx, &type_name) {
            return Some(unsupported);
        }
    }
    let type_info = validation_expr_type_info(ctx, expr)?;
    if let Some(type_name) = derived_type_name_from_type_info(&type_info) {
        if let Some(unsupported) = ownerless_finalizer_type_name(ctx, &type_name) {
            return Some(unsupported);
        }
    }
    ctx.type_layouts?
        .visible_ownerless_finalizer_for_polymorphic(ctx.scope_id, &type_info)
        .map(|layout| layout.name.clone())
}

fn assignment_type_names_match(
    ctx: &Ctx<'_>,
    declared_scope: ScopeId,
    declared: &str,
    actual: &str,
) -> bool {
    let Some(layouts) = ctx.type_layouts else {
        return declared.eq_ignore_ascii_case(actual);
    };
    let declared_layout = layouts
        .get_for_scope(declared_scope, declared)
        .or_else(|| layouts.get(declared));
    let actual_layout = layouts
        .get_for_scope(ctx.scope_id, actual)
        .or_else(|| layouts.get(actual));
    match (declared_layout, actual_layout) {
        (Some(declared), Some(actual)) => {
            layouts.canonical_key_for_layout(declared) == layouts.canonical_key_for_layout(actual)
        }
        _ => declared.eq_ignore_ascii_case(actual),
    }
}

fn assignment_type_is_same_or_extension(
    ctx: &Ctx<'_>,
    declared_scope: ScopeId,
    declared: &str,
    actual: &str,
) -> bool {
    let Some(layouts) = ctx.type_layouts else {
        return declared.eq_ignore_ascii_case(actual);
    };
    let declared_layout = layouts
        .get_for_scope(declared_scope, declared)
        .or_else(|| layouts.get(declared));
    let actual_layout = layouts
        .get_for_scope(ctx.scope_id, actual)
        .or_else(|| layouts.get(actual));
    match (declared_layout, actual_layout) {
        (Some(declared), Some(actual)) => layouts.is_same_or_extension_of(actual, declared),
        _ => declared.eq_ignore_ascii_case(actual),
    }
}

fn defined_assignment_type_matches(
    ctx: &Ctx<'_>,
    declared_scope: ScopeId,
    declared: &TypeInfo,
    actual: &TypeInfo,
) -> bool {
    fn kind_eq(a: Option<u8>, b: Option<u8>, default: u8) -> bool {
        a.unwrap_or(default) == b.unwrap_or(default)
    }

    match (declared, actual) {
        (TypeInfo::Derived(declared), TypeInfo::Derived(actual)) => {
            assignment_type_names_match(ctx, declared_scope, declared, actual)
        }
        (TypeInfo::Class(declared), TypeInfo::Class(actual) | TypeInfo::Derived(actual)) => {
            assignment_type_is_same_or_extension(ctx, declared_scope, declared, actual)
        }
        (TypeInfo::ClassStar, TypeInfo::ClassStar)
        | (TypeInfo::TypeStar, TypeInfo::TypeStar)
        | (TypeInfo::DoublePrecision, TypeInfo::DoublePrecision) => true,
        (TypeInfo::Character { .. }, TypeInfo::Character { .. }) => true,
        (TypeInfo::Integer { kind: a }, TypeInfo::Integer { kind: b }) => kind_eq(*a, *b, 4),
        (TypeInfo::Real { kind: a }, TypeInfo::Real { kind: b }) => kind_eq(*a, *b, 4),
        (TypeInfo::Real { kind }, TypeInfo::DoublePrecision)
        | (TypeInfo::DoublePrecision, TypeInfo::Real { kind }) => kind_eq(*kind, Some(8), 4),
        (TypeInfo::Complex { kind: a }, TypeInfo::Complex { kind: b }) => kind_eq(*a, *b, 4),
        (TypeInfo::Logical { kind: a }, TypeInfo::Logical { kind: b }) => kind_eq(*a, *b, 4),
        (TypeInfo::Enumeration(a), TypeInfo::Enumeration(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

fn assignment_candidate_scope<'a>(
    ctx: &'a Ctx<'_>,
    name: &str,
    owner_scope: ScopeId,
) -> Option<&'a Scope> {
    ctx.st
        .all_scopes()
        .iter()
        .find(|scope| {
            matches!(
                &scope.kind,
                ScopeKind::Function(candidate) | ScopeKind::Subroutine(candidate)
                    if candidate.eq_ignore_ascii_case(name)
            ) && scope.parent == Some(owner_scope)
        })
        .or_else(|| {
            ctx.st.all_scopes().iter().find(|scope| {
                matches!(
                    &scope.kind,
                    ScopeKind::Function(candidate) | ScopeKind::Subroutine(candidate)
                        if candidate.eq_ignore_ascii_case(name)
                )
            })
        })
}

fn validation_expr_rank(ctx: &Ctx<'_>, expr: &SpannedExpr) -> Option<usize> {
    use crate::ast::expr::SectionSubscript;

    match &expr.node {
        Expr::Name { name } => ctx.lookup(name).map(|symbol| symbol.attrs.array_spec.len()),
        Expr::ParenExpr { inner } | Expr::UnaryOp { operand: inner, .. } => {
            validation_expr_rank(ctx, inner)
        }
        Expr::ComponentAccess { base, .. } => {
            let base_rank = validation_expr_rank(ctx, base)?;
            let field_rank = leaf_field_layout(ctx, expr)?.field.dims.len();
            Some(base_rank + field_rank)
        }
        Expr::BinaryOp { left, right, .. } => match (
            validation_expr_rank(ctx, left),
            validation_expr_rank(ctx, right),
        ) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(rank), None) | (None, Some(rank)) => Some(rank),
            (None, None) => None,
        },
        Expr::ConditionalExpr { then_val, .. } => validation_expr_rank(ctx, then_val),
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let symbol = ctx.lookup(name)?;
            if matches!(symbol.kind, SymbolKind::DerivedType) {
                return Some(0);
            }
            if !symbol.attrs.array_spec.is_empty() {
                if args.is_empty() {
                    return Some(symbol.attrs.array_spec.len());
                }
                return Some(
                    args.iter()
                        .filter(|arg| match &arg.value {
                            SectionSubscript::Range { .. } => true,
                            SectionSubscript::Element(index) => {
                                validation_expr_rank(ctx, index).is_some_and(|rank| rank > 0)
                            }
                        })
                        .count(),
                );
            }
            matches!(
                symbol.kind,
                SymbolKind::Function | SymbolKind::NamedInterface
            )
            .then_some(symbol.attrs.result_rank as usize)
        }
        Expr::ArrayConstructor { .. } => Some(1),
        Expr::IntegerLiteral { .. }
        | Expr::RealLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::ComplexLiteral { .. }
        | Expr::BozLiteral { .. } => Some(0),
        Expr::NilArgument => None,
    }
}

fn assignment_uses_defined_assignment(
    ctx: &Ctx<'_>,
    target: &SpannedExpr,
    value: &SpannedExpr,
) -> bool {
    if validation_expr_rank(ctx, target) != Some(0) || validation_expr_rank(ctx, value) != Some(0) {
        return false;
    }
    let Some(lhs_type) = validation_expr_type_info(ctx, target) else {
        return false;
    };
    let Some(rhs_type) = validation_expr_type_info(ctx, value) else {
        return false;
    };

    let mut candidates: Vec<(String, ScopeId)> = Vec::new();
    if let Some(interface) = ctx.lookup("assignment(=)") {
        if matches!(interface.kind, SymbolKind::NamedInterface) {
            for name in &interface.arg_names {
                candidates.push((name.clone(), interface.scope));
            }
        }
    }

    candidates.into_iter().any(|(name, owner_scope)| {
        let Some(scope) = assignment_candidate_scope(ctx, &name, owner_scope) else {
            return false;
        };
        let declared_args: Vec<_> = scope
            .arg_order
            .iter()
            .filter_map(|name| scope.symbols.get(name))
            .collect();
        if declared_args.len() != 2 {
            return false;
        }
        if declared_args
            .iter()
            .any(|argument| !argument.attrs.array_spec.is_empty())
        {
            return false;
        }
        let Some(lhs_declared) = declared_args[0].type_info.as_ref() else {
            return false;
        };
        let Some(rhs_declared) = declared_args[1].type_info.as_ref() else {
            return false;
        };
        defined_assignment_type_matches(ctx, scope.id, lhs_declared, &lhs_type)
            && defined_assignment_type_matches(ctx, scope.id, rhs_declared, &rhs_type)
    })
}

fn allocate_option_expr<'a>(opts: &'a [IoControl], keyword: &str) -> Option<&'a SpannedExpr> {
    opts.iter()
        .find(|opt| {
            opt.keyword
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(keyword))
        })
        .map(|opt| &opt.value)
}

fn type_spec_dynamic_type_name(ctx: &Ctx<'_>, type_spec: &TypeSpec) -> Option<String> {
    match type_spec {
        TypeSpec::Type(name) | TypeSpec::Class(name) => Some(name.clone()),
        TypeSpec::TypeOf(name) | TypeSpec::ClassOf(name) => ctx
            .lookup(name)
            .and_then(|symbol| symbol.type_info.as_ref())
            .and_then(derived_type_name_from_type_info),
        _ => None,
    }
}

fn unsupported_allocate_dynamic_type(
    ctx: &Ctx<'_>,
    type_spec: Option<&TypeSpec>,
    opts: &[IoControl],
    item: &SpannedExpr,
) -> Option<String> {
    if let Some(type_name) = type_spec.and_then(|spec| type_spec_dynamic_type_name(ctx, spec)) {
        return ownerless_finalizer_type_name(ctx, &type_name);
    }
    if let Some(source) = allocate_option_expr(opts, "source") {
        return unsupported_polymorphic_ownership_from_expr(ctx, source);
    }
    if let Some(mold) = allocate_option_expr(opts, "mold") {
        return unsupported_polymorphic_ownership_from_expr(ctx, mold);
    }
    derived_type_name_for_expr(ctx, item).and_then(|name| ownerless_finalizer_type_name(ctx, &name))
}

fn call_argument_expr<'a>(
    args: &'a [crate::ast::expr::Argument],
    position: usize,
    keyword: &str,
) -> Option<&'a SpannedExpr> {
    let argument = args
        .iter()
        .find(|arg| {
            arg.keyword
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(keyword))
        })
        .or_else(|| {
            args.iter()
                .filter(|arg| arg.keyword.is_none())
                .nth(position)
        })?;
    let crate::ast::expr::SectionSubscript::Element(expr) = &argument.value else {
        return None;
    };
    Some(expr)
}

fn validate_move_alloc_polymorphic_ownership(
    ctx: &mut Ctx<'_>,
    args: &[crate::ast::expr::Argument],
) {
    let Some(source) = call_argument_expr(args, 0, "from") else {
        return;
    };
    let Some(target) = call_argument_expr(args, 1, "to") else {
        return;
    };
    if !polymorphic_allocatable_target(ctx, target) {
        return;
    }
    if let Some(type_name) = unsupported_polymorphic_ownership_from_expr(ctx, source) {
        reject_context_dependent_polymorphic_ownership(ctx, source.span, &type_name);
    }
}

fn reject_context_dependent_polymorphic_ownership(ctx: &mut Ctx<'_>, span: Span, type_name: &str) {
    ctx.error(
        span,
        format!(
            "polymorphic ownership of locally declared finalizable type '{}' cannot preserve its local FINAL procedure; move the type and FINAL procedure to module scope",
            type_name
        ),
    );
}

fn layout_component_type_info(
    layout: &crate::sema::type_layout::TypeLayout,
    component: &str,
    layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<TypeInfo> {
    if let Some(field) = layout.field(component) {
        return Some(field.type_info.clone());
    }

    let mut parent = layout.parent.as_deref();
    while let Some(parent_name) = parent {
        let parent_layout = layouts.get(parent_name)?;
        if parent_name.eq_ignore_ascii_case(component)
            || parent_layout.name.eq_ignore_ascii_case(component)
        {
            return Some(TypeInfo::Derived(parent_layout.name.clone()));
        }
        parent = parent_layout.parent.as_deref();
    }

    None
}

fn resolve_component_access_type(
    ctx: &Ctx<'_>,
    expr: &crate::ast::expr::SpannedExpr,
) -> Result<Option<String>, (Span, String, String)> {
    let Expr::ComponentAccess { base, component } = &expr.node else {
        return Ok(None);
    };
    let Some(base_type) = derived_type_name_for_expr(ctx, base) else {
        return Ok(None);
    };
    let Some(layouts) = ctx.type_layouts else {
        return Ok(None);
    };
    let Some(layout) = layouts.get(&base_type) else {
        return Ok(None);
    };
    let Some(type_info) = layout_component_type_info(layout, component, layouts) else {
        if layout.bound_proc(component).is_some() {
            return Ok(None);
        }
        return Err((expr.span, component.clone(), base_type));
    };
    Ok(derived_type_name_from_type_info(&type_info))
}

fn validate_component_access(ctx: &mut Ctx<'_>, expr: &crate::ast::expr::SpannedExpr) {
    if let Err((span, component, base_type)) = resolve_component_access_type(ctx, expr) {
        ctx.error(
            span,
            format!(
                "unknown component '{}' for derived type '{}'",
                component, base_type
            ),
        );
    }
}

/// Check that an assignment target is modifiable (not intent(in), not parameter).
/// Handles component access (x%field) and array elements (a(i)) — the base
/// variable's intent/parameter status applies to all parts.
fn validate_assignment_target(ctx: &mut Ctx, target: &crate::ast::expr::SpannedExpr, span: Span) {
    if let Some(name) = extract_base_name(target) {
        // F2018 §11.1.3.3: an associate-name aliases its selector and
        // shadows any same-named outer symbol inside the construct body.
        // The selector's writability — not the outer symbol's parameter
        // or intent — governs whether the assignment is legal.
        if ctx.is_associate_name(&name) {
            return;
        }
        let (is_intent_in, is_parameter, is_pointer) =
            if let Some(attrs) = ctx.block_binding_attrs(&name) {
                (attrs.intent_in, attrs.parameter, attrs.pointer)
            } else {
                ctx.lookup(&name)
                    .map(|sym| {
                        (
                            matches!(sym.attrs.intent, Some(Intent::In)),
                            sym.attrs.parameter,
                            sym.attrs.pointer,
                        )
                    })
                    .unwrap_or((false, false, false))
            };
        let writes_through_pointer_target = is_pointer && !matches!(target.node, Expr::Name { .. });
        if is_intent_in && !writes_through_pointer_target {
            ctx.error(
                span,
                format!("cannot assign to intent(in) variable '{}'", name),
            );
        }
        if is_parameter {
            ctx.error(span, format!("cannot assign to named constant '{}'", name));
        }
    }
}

/// Validate pointer assignment: LHS must be pointer, RHS must be target/pointer.
/// Validate that an ALLOCATE/DEALLOCATE item is allocatable or pointer.
///
/// For a component access like `pools(i)%tokens(n)`, the target is
/// the `tokens` field — not the `pools` base.  Resolve the leaf
/// component through the type-layout registry and check its own
/// attributes.  Bare-name targets still get the symbol attribute
/// check.  If the chain can't be resolved (registry missing, cross-
/// TU stale .amod, etc.) we skip rather than produce a misleading
/// error.
/// Check if a call in a pure procedure is to a known impure procedure.
/// Symbol-level pure tracking isn't yet wired into the symbol table,
/// so this is conservative: we warn if the callee resolves to an
/// external procedure (whose body we cannot inspect).  I/O, STOP,
/// and SAVE violations are caught statement-level in validate_stmt.
/// Walk an expression tree and check any function calls against the
/// pure-call constraint.  Catches `r = impure_fn()` which is an
/// expression-level call, not a `Stmt::Call`.
fn uses_internal_character_file(ctx: &Ctx, controls: &[IoControl]) -> bool {
    let Some(unit) = controls.iter().find(|control| {
        control
            .keyword
            .as_deref()
            .is_none_or(|kw| kw.eq_ignore_ascii_case("unit"))
    }) else {
        return false;
    };

    if matches!(&unit.value.node, Expr::Name { name } if name == "*") {
        return false;
    }

    match &unit.value.node {
        Expr::StringLiteral { .. } => true,
        Expr::Name { name } => ctx
            .lookup(name)
            .and_then(|sym| sym.type_info.as_ref())
            .is_some_and(|ty| matches!(ty, TypeInfo::Character { .. })),
        Expr::ParenExpr { inner } => uses_internal_character_file(
            ctx,
            &[IoControl {
                keyword: None,
                value: (**inner).clone(),
            }],
        ),
        _ => expr_type(&unit.value, ctx.st).is_character(),
    }
}

/// F2023 16.9.202 SYSTEM_CLOCK argument restrictions: every integer
/// argument must have a kind no smaller than the default integer kind,
/// and all integer arguments must have the same kind. COUNT_RATE may
/// be real (exempt from both rules). Only enforced when SYSTEM_CLOCK
/// resolves to the intrinsic (a user procedure of the same name is
/// exempt). Gated by --std=f2023 — these are conformance diagnostics,
/// silent under f2018.
fn validate_system_clock_args(ctx: &mut Ctx, args: &[crate::ast::expr::Argument], span: Span) {
    use crate::sema::types::FortranType;
    if ctx.std.is_none_or(|s| s < FortranStandard::F2023) {
        return;
    }
    const FORMALS: [&str; 3] = ["count", "count_rate", "count_max"];
    let mut int_kinds: Vec<u8> = Vec::new();
    let mut positional = 0usize;
    for arg in args {
        let formal = match arg.keyword.as_deref() {
            Some(kw) => kw.to_ascii_lowercase(),
            None => {
                let f = FORMALS.get(positional).map(|s| s.to_string());
                positional += 1;
                match f {
                    Some(f) => f,
                    None => continue,
                }
            }
        };
        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
            continue;
        };
        if let FortranType::Integer { kind } = conditional_operand_type(ctx, e) {
            if kind < 4 {
                ctx.error(
                    e.span,
                    format!(
                        "SYSTEM_CLOCK argument '{}' has integer kind {} smaller than the \
                         default integer kind (F2023 16.9.202)",
                        formal, kind
                    ),
                );
            } else {
                int_kinds.push(kind);
            }
        }
    }
    if int_kinds.len() >= 2 && int_kinds.iter().any(|k| *k != int_kinds[0]) {
        ctx.error(
            span,
            "SYSTEM_CLOCK integer arguments must all have the same kind (F2023 16.9.202)",
        );
    }
}

fn call_target_function_name(
    ctx: &Ctx<'_>,
    callee: &crate::ast::expr::SpannedExpr,
) -> Option<String> {
    let Expr::Name { name } = &callee.node else {
        return None;
    };
    let sym = ctx.lookup(name)?;
    matches!(sym.kind, SymbolKind::Function).then(|| sym.name.clone())
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
    let callee_name = if let Expr::Name { name } = &callee.node {
        name.clone()
    } else {
        return;
    };

    // For each actual argument, check if it's an lvalue when the dummy requires out/inout.
    // We can only check this if the callee's dummy arg info is in the symbol table.
    // For now, check the simpler case: passing a literal or parameter to ANY subroutine arg.
    for arg in args {
        let actual = match &arg.value {
            crate::ast::expr::SectionSubscript::Element(e) => e,
            _ => continue,
        };
        // Check if actual is a literal (not an lvalue).
        let is_literal = matches!(
            actual.node,
            Expr::IntegerLiteral { .. }
                | Expr::RealLiteral { .. }
                | Expr::StringLiteral { .. }
                | Expr::LogicalLiteral { .. }
                | Expr::ComplexLiteral { .. }
        );
        // Check if actual is a named constant (parameter).
        let is_parameter = if let Some(name) = extract_base_name(actual) {
            ctx.lookup(&name)
                .map(|s| s.attrs.parameter)
                .unwrap_or(false)
        } else {
            false
        };

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
    let errors: Vec<(Span, String)> = ctx
        .labels_referenced
        .iter()
        .filter(|(label, _)| !ctx.labels_defined.contains(label))
        .map(|(label, span)| {
            (
                *span,
                format!("GOTO target label {} not defined in this scope", label),
            )
        })
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
                            ctx.error(
                                sub.span,
                                format!(
                                "ASSIGNMENT({}) interface must contain subroutines, not functions",
                                "="
                            ),
                            );
                            continue;
                        }
                        // Operator functions: unary = 1 arg, binary = 2 args.
                        let nargs = args.len();
                        if !(1..=2).contains(&nargs) {
                            ctx.error(
                                sub.span,
                                format!(
                                "operator interface function must have 1 or 2 arguments, got {}",
                                nargs
                            ),
                            );
                        }
                        // All arguments must be intent(in) — checked by looking at decls.
                        // Deferred: would need to walk the function's decls to check intent.
                    }
                    ProgramUnit::Subroutine { args, .. } => {
                        if !is_assignment {
                            ctx.error(
                                sub.span,
                                "operator interface must contain functions, not subroutines",
                            );
                            continue;
                        }
                        // Assignment subroutines must have exactly 2 arguments.
                        if args.len() != 2 {
                            ctx.error(
                                sub.span,
                                format!(
                                "ASSIGNMENT(=) interface subroutine must have 2 arguments, got {}",
                                args.len()
                            ),
                            );
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
            ctx.error(
                span,
                format!(
                    "type-bound procedure '{}' is DEFERRED but type '{}' is not ABSTRACT",
                    tbp.name, name
                ),
            );
        }

        // PASS and NOPASS are mutually exclusive.
        let has_pass = tbp.attrs.iter().any(|a| {
            let lower = a.to_lowercase();
            lower == "pass" || lower.starts_with("pass(")
        });
        let has_nopass = tbp.attrs.iter().any(|a| a.eq_ignore_ascii_case("nopass"));
        if has_pass && has_nopass {
            ctx.error(
                span,
                format!(
                    "type-bound procedure '{}' cannot have both PASS and NOPASS",
                    tbp.name
                ),
            );
        }

        // Deferred procedures must declare an explicit interface.
        if is_deferred && tbp.interface.is_none() {
            ctx.error(
                span,
                format!(
                    "DEFERRED type-bound procedure '{}' must specify an interface",
                    tbp.name
                ),
            );
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
    let frame: HashSet<String> = assocs
        .iter()
        .filter_map(|(n, _)| {
            if n.is_empty() {
                None
            } else {
                Some(n.to_lowercase())
            }
        })
        .collect();
    ctx.associate_frames.push(frame);
    validate_stmts(ctx, body);
    ctx.associate_frames.pop();
}

fn block_binding_frame(decls: &[SpannedDecl]) -> HashMap<String, BlockBindingAttrs> {
    let mut frame = HashMap::new();
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl {
                attrs, entities, ..
            } => {
                let binding_attrs = block_attrs_from_decl(attrs.as_slice());
                for entity in entities {
                    frame.insert(entity.name.to_lowercase(), binding_attrs);
                }
            }
            Decl::ParameterStmt { pairs } => {
                for (name, _) in pairs {
                    frame.insert(
                        name.to_lowercase(),
                        BlockBindingAttrs {
                            parameter: true,
                            ..BlockBindingAttrs::default()
                        },
                    );
                }
            }
            _ => {}
        }
    }
    frame
}

fn block_attrs_from_decl(attrs: &[Attribute]) -> BlockBindingAttrs {
    let mut out = BlockBindingAttrs::default();
    for attr in attrs {
        match attr {
            Attribute::Parameter => out.parameter = true,
            Attribute::Pointer => out.pointer = true,
            Attribute::Intent(crate::ast::decl::Intent::In) => out.intent_in = true,
            _ => {}
        }
    }
    out
}

/// Extract the base variable name from an expression (handling subscripts and components).
pub(super) fn extract_base_name(expr: &crate::ast::expr::SpannedExpr) -> Option<String> {
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
fn check_implicit_none(
    ctx: &mut Ctx,
    stmts: &[SpannedStmt],
    decls: &[crate::ast::decl::SpannedDecl],
) {
    if !ctx.st.is_implicit_none(ctx.scope_id) {
        return;
    }

    // Collect declared names in this scope (from declarations).
    let mut declared: std::collections::HashSet<String> = std::collections::HashSet::new();
    extend_declared_names_from_decls(&mut declared, decls);
    extend_declared_names_from_namelist_stmts(&mut declared, stmts);
    // Also scan for INTERFACE blocks — function/subroutine names
    // declared in interfaces are valid in the current scope.
    // The interface bodies are stored as program units in the
    // ifaces/contains lists, not in decls. But the symbol table
    // should have them via resolve. We also check decls for
    // EXTERNAL statements.
    for decl in decls {
        if let Decl::TypeDecl {
            attrs, entities, ..
        } = &decl.node
        {
            if attrs.iter().any(|a| matches!(a, Attribute::External)) {
                for e in entities {
                    declared.insert(e.name.to_lowercase());
                }
            }
        }
    }

    let mut undeclared = Vec::new();
    let mut resolution_cache: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let outer_implicit_letters: std::collections::HashSet<char> = std::collections::HashSet::new();
    for stmt in stmts {
        walk_stmt_for_undeclared(
            ctx.st,
            ctx.scope_id,
            stmt,
            &declared,
            &outer_implicit_letters,
            &mut resolution_cache,
            &mut undeclared,
        );
    }

    // Deduplicate by name (only report each undeclared name once).
    let mut reported: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, span) in &undeclared {
        let key = name.to_lowercase();
        if reported.insert(key) {
            ctx.error(
                *span,
                format!(
                    "variable '{}' used but not declared (IMPLICIT NONE is active)",
                    name
                ),
            );
        }
    }
}

fn extend_declared_names_from_decls(
    declared: &mut std::collections::HashSet<String>,
    decls: &[crate::ast::decl::SpannedDecl],
) {
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
}

fn extend_declared_names_from_ifaces(
    declared: &mut std::collections::HashSet<String>,
    ifaces: &[crate::ast::unit::SpannedUnit],
) {
    use crate::ast::unit::{InterfaceBody, ProgramUnit};

    for iface in ifaces {
        let ProgramUnit::InterfaceBlock { bodies, .. } = &iface.node else {
            continue;
        };
        for body in bodies {
            match body {
                InterfaceBody::Subprogram(sub) => match &sub.node {
                    ProgramUnit::Function { name, .. } | ProgramUnit::Subroutine { name, .. } => {
                        declared.insert(name.to_lowercase());
                    }
                    _ => {}
                },
                InterfaceBody::ModuleProcedure(names) => {
                    for name in names {
                        declared.insert(name.to_lowercase());
                    }
                }
            }
        }
    }
}

fn extend_declared_names_from_namelist_stmts(
    declared: &mut std::collections::HashSet<String>,
    stmts: &[SpannedStmt],
) {
    for stmt in stmts {
        if let Stmt::Namelist { groups } = &stmt.node {
            for (name, _) in groups {
                declared.insert(name.to_lowercase());
            }
        }
    }
}

fn walk_stmt_for_undeclared(
    st: &SymbolTable,
    scope_id: ScopeId,
    stmt: &SpannedStmt,
    declared: &std::collections::HashSet<String>,
    implicit_letters: &std::collections::HashSet<char>,
    resolution_cache: &mut std::collections::HashMap<String, bool>,
    undeclared: &mut Vec<(String, Span)>,
) {
    macro_rules! chk {
        ($e:expr) => {
            check_expr_names(
                st,
                scope_id,
                $e,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            )
        };
    }
    macro_rules! recurse {
        ($s:expr) => {
            walk_stmt_for_undeclared(
                st,
                scope_id,
                $s,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            )
        };
    }
    match &stmt.node {
        Stmt::Assignment { target, value } => {
            chk!(target);
            chk!(value);
        }
        Stmt::PointerAssignment { target, value, .. } => {
            chk!(target);
            chk!(value);
        }
        Stmt::Print { items, .. } => {
            for item in items {
                chk!(item);
            }
        }
        Stmt::Write {
            items, controls, ..
        } => {
            for item in items {
                chk!(item);
            }
            for ctrl in controls {
                chk!(&ctrl.value);
            }
        }
        Stmt::Read {
            items, controls, ..
        } => {
            for item in items {
                chk!(item);
            }
            for ctrl in controls {
                chk!(&ctrl.value);
            }
        }
        Stmt::IfConstruct {
            condition,
            then_body,
            else_ifs,
            else_body,
            ..
        } => {
            chk!(condition);
            for s in then_body {
                recurse!(s);
            }
            for (cond, body) in else_ifs {
                chk!(cond);
                for s in body {
                    recurse!(s);
                }
            }
            if let Some(body) = else_body {
                for s in body {
                    recurse!(s);
                }
            }
        }
        Stmt::IfStmt { condition, action } => {
            chk!(condition);
            recurse!(action);
        }
        Stmt::DoLoop { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::DoConcurrent { body, .. } => {
            for s in body {
                recurse!(s);
            }
        }
        Stmt::Block {
            uses,
            ifaces,
            implicit,
            decls,
            body,
            ..
        } => {
            // F2018 §11.1.4: a BLOCK construct establishes its own
            // scope with an independent implicit-typing environment.
            // Layer the block's declared names AND any IMPLICIT
            // statements over the inherited rules; the local set
            // does not leak back out.
            let mut block_declared = declared.clone();
            block_declared.extend(block_use_imported_names(st, uses));
            extend_declared_names_from_decls(&mut block_declared, decls);
            extend_declared_names_from_ifaces(&mut block_declared, ifaces);
            extend_declared_names_from_namelist_stmts(&mut block_declared, body);
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
                    resolution_cache,
                    undeclared,
                );
            }
        }
        Stmt::SelectCase {
            selector, cases, ..
        } => {
            chk!(selector);
            for case in cases {
                for s in &case.body {
                    recurse!(s);
                }
            }
        }
        Stmt::Call { args, .. } => {
            for arg in args {
                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                    chk!(e);
                }
            }
        }
        Stmt::Labeled { stmt: inner, .. } => {
            recurse!(inner);
        }
        Stmt::WhereConstruct {
            mask,
            body,
            elsewhere,
            ..
        } => {
            chk!(mask);
            for s in body {
                recurse!(s);
            }
            for (m, b) in elsewhere {
                if let Some(m) = m {
                    chk!(m);
                }
                for s in b {
                    recurse!(s);
                }
            }
        }
        _ => {}
    }
}

fn block_use_imported_names(
    st: &SymbolTable,
    uses: &[crate::ast::decl::SpannedDecl],
) -> std::collections::HashSet<String> {
    use crate::ast::decl::OnlyItem;
    use crate::sema::symtab::Access;

    let mut imported = std::collections::HashSet::new();
    for use_decl in uses {
        let crate::ast::decl::Decl::UseStmt {
            module,
            renames,
            only,
            ..
        } = &use_decl.node
        else {
            continue;
        };
        if let Some(only_items) = only {
            for item in only_items {
                match item {
                    OnlyItem::Name(name) => {
                        imported.insert(name.to_lowercase());
                    }
                    OnlyItem::Generic(name) => {
                        imported.insert(name.to_lowercase());
                    }
                    OnlyItem::Rename(rename) => {
                        imported.insert(rename.local.to_lowercase());
                    }
                }
            }
            continue;
        }

        if let Some(scope_id) = st.find_module_scope(module) {
            for sym in st.scope(scope_id).symbols.values() {
                if sym.attrs.access != Access::Private {
                    imported.insert(sym.name.to_lowercase());
                }
            }
        }
        for rename in renames {
            imported.insert(rename.local.to_lowercase());
        }
    }
    imported
}

/// Walk an expression and collect undeclared Name references.
fn check_expr_names(
    st: &SymbolTable,
    scope_id: ScopeId,
    expr: &crate::ast::expr::SpannedExpr,
    declared: &std::collections::HashSet<String>,
    implicit_letters: &std::collections::HashSet<char>,
    resolution_cache: &mut std::collections::HashMap<String, bool>,
    undeclared: &mut Vec<(String, Span)>,
) {
    match &expr.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            // Skip format specifier * (appears in WRITE(*, *) / READ(*, *)).
            if key == "*" {
                return;
            }
            if declared.contains(&key) {
                return;
            }
            if is_intrinsic_name(&key) {
                return;
            }
            // F2018 §11.1.4: a BLOCK-scoped IMPLICIT statement gives
            // names whose first letter is in the covered range an
            // implicit type, even if the enclosing scope is
            // IMPLICIT NONE.
            if let Some(first) = key.chars().next() {
                if implicit_letters.contains(&first.to_ascii_lowercase()) {
                    return;
                }
            }
            if *resolution_cache
                .entry(key.clone())
                .or_insert_with(|| st.lookup_in(scope_id, &key).is_some())
            {
                return;
            }
            undeclared.push((name.clone(), expr.span));
        }
        Expr::BinaryOp { left, right, .. } => {
            check_expr_names(
                st,
                scope_id,
                left,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            );
            check_expr_names(
                st,
                scope_id,
                right,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            );
        }
        Expr::UnaryOp { operand, .. } => {
            check_expr_names(
                st,
                scope_id,
                operand,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            );
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
            check_expr_names(
                st,
                scope_id,
                callee,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            );
            for arg in args {
                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                    check_expr_names(
                        st,
                        scope_id,
                        e,
                        declared,
                        implicit_letters,
                        resolution_cache,
                        undeclared,
                    );
                }
            }
        }
        Expr::ComponentAccess { base, .. } => {
            check_expr_names(
                st,
                scope_id,
                base,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            );
        }
        Expr::ParenExpr { inner } => {
            check_expr_names(
                st,
                scope_id,
                inner,
                declared,
                implicit_letters,
                resolution_cache,
                undeclared,
            );
        }
        _ => {}
    }
}

/// Argument-count bounds for intrinsics where F2023 16.9 pins them
/// unambiguously, counting positional and keyword actuals alike.
/// `None` max means unbounded (the MAX/MIN family). Names absent
/// from this table are NOT arity-checked — extend it only with a
/// standard citation in hand: a wrong bound here rejects valid
/// programs, which is worse than the silent-acceptance gap it closes.
fn intrinsic_arity(name: &str) -> Option<(usize, Option<usize>)> {
    Some(match name {
        // Exactly one argument.
        "abs"
        | "iabs"
        | "dabs"
        | "cabs"
        | "conjg"
        | "aimag"
        | "dimag"
        | "acos"
        | "asin"
        | "acosh"
        | "asinh"
        | "atanh"
        | "cos"
        | "sin"
        | "tan"
        | "cosh"
        | "sinh"
        | "tanh"
        | "exp"
        | "log"
        | "log10"
        | "sqrt"
        | "dsqrt"
        | "gamma"
        | "log_gamma"
        | "erf"
        | "erfc"
        | "fraction"
        | "exponent"
        | "trim"
        | "adjustl"
        | "adjustr"
        | "allocated"
        | "present"
        | "kind"
        | "precision"
        | "range"
        | "radix"
        | "digits"
        | "huge"
        | "tiny"
        | "epsilon"
        | "maxexponent"
        | "minexponent"
        | "bit_size"
        | "popcnt"
        | "poppar"
        | "leadz"
        | "trailz"
        | "not"
        | "sngl"
        | "float"
        | "dfloat"
        | "idint"
        | "ifix"
        | "idnint"
        | "dnint"
        | "dint"
        | "acosd"
        | "asind"
        | "cosd"
        | "sind"
        | "tand"
        | "acospi"
        | "asinpi"
        | "cospi"
        | "sinpi"
        | "tanpi"
        | "selected_logical_kind"
        | "selected_int_kind"
        | "selected_char_kind"
        | "is_iostat_end"
        | "is_iostat_eor"
        | "c_loc"
        | "c_funloc"
        | "c_sizeof"
        | "new_line"
        | "transpose"
        | "dble"
        | "random_number"
        | "cpu_time"
        | "ieee_is_nan"
        | "ieee_is_finite"
        | "ieee_is_normal" => (1, Some(1)),
        // Exactly two.
        "atan2" | "atan2d" | "atan2pi" | "hypot" | "mod" | "modulo" | "sign" | "dim" | "dprod"
        | "lge" | "lgt" | "lle" | "llt" | "bge" | "bgt" | "ble" | "blt" | "ishft" | "shiftl"
        | "shiftr" | "shifta" | "ibset" | "ibclr" | "btest" | "iand" | "ior" | "ieor"
        | "matmul" | "dot_product" | "scale" | "repeat" | "ieee_copy_sign" | "ieee_unordered" => {
            (2, Some(2))
        }
        // Exactly three.
        "ibits" | "merge" | "merge_bits" | "dshiftl" | "dshiftr" | "unpack" | "spread" => {
            (3, Some(3))
        }
        // Optional-argument ranges (F2023 16.9 per-procedure).
        "atan" | "atand" | "atanpi" | "aint" | "anint" | "nint" | "int" | "real" | "logical"
        | "char" | "ichar" | "achar" | "iachar" | "len" | "len_trim" | "floor" | "ceiling"
        | "maskl" | "maskr" | "shape" | "storage_size" | "associated" | "any" | "all" | "norm2"
        | "f_c_string" | "iall" | "iany" | "iparity" | "parity" => (1, Some(2)),
        "cmplx" | "size" | "lbound" | "ubound" | "sum" | "product" | "maxval" | "minval"
        | "count" | "selected_real_kind" => (1, Some(3)),
        "ishftc" | "pack" | "transfer" | "c_f_strpointer" | "cshift" => (2, Some(3)),
        "index" | "scan" | "verify" | "reshape" | "eoshift" => (2, Some(4)),
        "null" => (0, Some(1)),
        "mvbits" => (5, Some(5)),
        "max" | "min" | "max0" | "min0" | "max1" | "min1" | "amax0" | "amin0" | "amax1"
        | "amin1" | "dmax1" | "dmin1" => (2, None),
        "maxloc" | "minloc" => (1, Some(5)),
        "findloc" => (2, Some(6)),
        "move_alloc" => (2, Some(4)),
        "system_clock" => (0, Some(3)),
        "date_and_time" => (0, Some(4)),
        "random_seed" => (0, Some(3)),
        "execute_command_line" => (1, Some(5)),
        "get_command_argument" => (1, Some(5)),
        "get_environment_variable" => (1, Some(6)),
        "command_argument_count" | "compiler_version" | "compiler_options" => (0, Some(0)),
        "split" => (3, Some(4)),
        "c_f_pointer" => (2, Some(4)),
        "c_associated" => (1, Some(2)),
        "next" | "previous" => (1, Some(2)),
        _ => return None,
    })
}

/// Intrinsics that are SUBROUTINES (16.9): referencing one in an
/// expression used to compile and die at link ("undefined symbol:
/// system_clock"); CALLing a function intrinsic was equally silent.
fn intrinsic_is_subroutine(name: &str) -> bool {
    matches!(
        name,
        "system_clock"
            | "date_and_time"
            | "cpu_time"
            | "random_number"
            | "random_seed"
            | "move_alloc"
            | "mvbits"
            | "execute_command_line"
            | "get_command_argument"
            | "get_environment_variable"
            | "split"
            | "tokenize"
            | "c_f_pointer"
            | "c_f_strpointer"
    )
}

fn intrinsic_not_implemented(name: &str) -> bool {
    matches!(name, "iall" | "iany" | "iparity" | "parity")
}

/// Reject a reference to an intrinsic with the wrong form (subroutine
/// in function position or vice versa) or an argument count outside
/// the standard's bounds (`atan2(1.0)` used to compile silently and
/// produce garbage). Fires only when the name actually resolves to
/// the intrinsic — any visible user symbol (procedure, array, dummy)
/// shadows it and is exempt.
pub(super) fn check_intrinsic_call_arity(
    ctx: &mut Ctx<'_>,
    span: Span,
    name: &str,
    nargs: usize,
    is_call: bool,
) {
    let key = name.to_lowercase();
    if ctx.lookup(&key).is_some() || !is_intrinsic_name(&key) {
        return;
    }
    let is_sub = intrinsic_is_subroutine(&key);
    if !is_call && is_sub {
        ctx.error(
            span,
            format!("intrinsic '{}' is a subroutine, not a function", key),
        );
        return;
    }
    if is_call && !is_sub {
        ctx.error(
            span,
            format!(
                "intrinsic '{}' is a function; reference it in an expression, not a CALL",
                key
            ),
        );
        return;
    }
    if intrinsic_not_implemented(&key) {
        ctx.error(
            span,
            format!("intrinsic '{}' is recognized but not implemented", key),
        );
        return;
    }
    let Some((min, max)) = intrinsic_arity(&key) else {
        return;
    };
    if nargs >= min && max.is_none_or(|m| nargs <= m) {
        return;
    }
    let expect = match (min, max) {
        (a, Some(b)) if a == b => a.to_string(),
        (a, Some(b)) => format!("{} to {}", a, b),
        (a, None) => format!("at least {}", a),
    };
    let noun = if expect == "1" {
        "argument"
    } else {
        "arguments"
    };
    ctx.error(
        span,
        format!(
            "intrinsic '{}' takes {} {}, got {}",
            key, expect, noun, nargs
        ),
    );
}

pub fn is_intrinsic_name(name: &str) -> bool {
    matches!(
        name,
        "abs" | "iabs" | "dabs" | "cabs" | "acos" | "asin" | "atan" | "atan2" |
        "hypot" | "anint" | "dnint" | "aint" | "dint" | "norm2" |
        "cos" | "sin" | "tan" | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh" |
        // F2023 degree trig (16.9) and half-revolution trig.
        "acosd" | "asind" | "atand" | "atan2d" | "cosd" | "sind" | "tand" |
        "acospi" | "asinpi" | "atanpi" | "atan2pi" | "cospi" | "sinpi" | "tanpi" |
        "selected_logical_kind" |
        "exp" | "log" | "log10" | "sqrt" | "dsqrt" |
        "mod" | "modulo" | "max" | "min" | "sign" | "dim" |
        "int" | "nint" | "real" | "dble" | "logical" | "cmplx" | "conjg" |
        "aimag" | "dimag" | "char" | "ichar" | "achar" | "iachar" |
        "len" | "len_trim" | "trim" | "adjustl" | "adjustr" |
        "index" | "scan" | "verify" | "repeat" | "lge" | "lgt" | "lle" | "llt" |
        "kind" | "selected_int_kind" | "selected_real_kind" | "selected_char_kind" |
        "size" | "shape" | "rank" | "lbound" | "ubound" | "allocated" | "associated" |
        "present" | "same_type_as" | "merge" | "pack" | "unpack" | "spread" | "reshape" | "cshift" | "eoshift" |
        "sum" | "product" | "maxval" | "minval" | "maxloc" | "minloc" | "findloc" | "count" | "any" | "all" |
        "iall" | "iany" | "iparity" | "parity" |
        "ieee_support_inf" | "ieee_support_nan" | "ieee_support_subnormal" |
        "ieee_support_divide" | "ieee_support_sqrt" | "ieee_support_io" |
        "ieee_support_rounding" | "ieee_support_flag" | "ieee_support_halting" |
        "ieee_support_standard" | "ieee_support_underflow_control" |
        "ieee_is_normal" | "ieee_class" | "ieee_unordered" | "ieee_copy_sign" |
        "ieee_logb" | "ieee_rint" | "ieee_scalb" | "ieee_next_after" |
        "ieee_max" | "ieee_min" | "ieee_max_mag" | "ieee_min_mag" |
        "ieee_max_num" | "ieee_min_num" | "ieee_max_num_mag" | "ieee_min_num_mag" |
        "matmul" | "dot_product" | "transpose" |
        "huge" | "tiny" | "epsilon" | "precision" | "range" | "radix" |
        "maxexponent" | "minexponent" | "digits" | "bit_size" | "storage_size" |
        "floor" | "ceiling" | "fraction" | "exponent" | "scale" |
        "gamma" | "log_gamma" | "erf" | "erfc" |
        "ibset" | "ibclr" | "ibits" | "btest" | "iand" | "ior" | "ieor" | "not" |
        "ishft" | "ishftc" | "shiftl" | "shiftr" | "shifta" |
        "popcnt" | "poppar" | "leadz" | "trailz" |
        "mvbits" | "transfer" | "bge" | "bgt" | "ble" | "blt" |
        "dshiftl" | "dshiftr" | "maskl" | "maskr" | "merge_bits" |
        "new_line" | "null" | "move_alloc" | "next" | "previous" |
        "system_clock" | "date_and_time" | "cpu_time" | "random_number" | "random_seed" |
        // F2023 string-parsing subroutines.
        "split" | "tokenize" |
        "command_argument_count" | "get_command_argument" | "get_environment_variable" |
        "execute_command_line" | "compiler_version" | "compiler_options" |
        "is_iostat_end" | "is_iostat_eor" |
        "c_loc" | "c_funloc" | "c_f_pointer" | "c_associated" | "c_sizeof" |
        "c_f_strpointer" | "f_c_string" |
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
        let rr = resolve::resolve_file(&units, &[], crate::target::TargetLayout::LP64).unwrap();
        let st = rr.st;
        validate_file(&units, &st)
    }

    fn errors_from(src: &str) -> Vec<String> {
        validate_source(src)
            .iter()
            .filter(|d| d.kind == DiagKind::Error)
            .map(|d| d.msg.clone())
            .collect()
    }

    fn errors_with_std(src: &str, std: FortranStandard) -> Vec<String> {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        let rr = resolve::resolve_file(&units, &[], crate::target::TargetLayout::LP64).unwrap();
        let st = rr.st;
        validate_file_with_std(&units, &st, Some(std))
            .iter()
            .filter(|d| d.kind == DiagKind::Error)
            .map(|d| d.msg.clone())
            .collect()
    }

    #[test]
    fn implicit_none_accepts_namelist_group_in_nml_control() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: x
  namelist /expected/ x
  read(*, nml=expected)
end program
",
        );
        assert!(!errs.iter().any(|e| e.contains("expected")), "{:?}", errs);
    }

    #[test]
    fn rejects_recursive_inline_storage_cycles() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: node_t
    type(node_t) :: child
  end type
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("infinite inline storage")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_pointer_and_allocatable_recursive_components() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: node_t
    type(node_t), pointer :: next
    type(node_t), allocatable :: children(:)
  end type
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("infinite inline storage")
                    || err.contains("locally declared derived type")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_local_open_dynamic_components() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: node_t
    class(*), allocatable :: child
    class(*), pointer :: link
  end type
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("locally declared derived type")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_polymorphic_ownership_of_local_finalizable_types() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  type(payload_t) :: source
  class(*), allocatable :: typed, sourced, molded, assigned
  allocate(payload_t :: typed)
  allocate(sourced, source=source)
  allocate(molded, mold=source)
  assigned = source
contains
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("cannot preserve its local FINAL procedure"))
                .count(),
            4,
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_polymorphic_transfer_and_runtime_cloning_of_local_finalizers() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  type(payload_t), allocatable :: concrete
  class(*), allocatable :: moved
  allocate(concrete)
  call move_alloc(concrete, moved)
  call clone(moved)
contains
  subroutine clone(source)
    class(*), intent(in) :: source
    class(*), allocatable :: sourced, molded, assigned
    allocate(sourced, source=source)
    allocate(molded, mold=source)
    assigned = source
  end subroutine
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("cannot preserve its local FINAL procedure"))
                .count(),
            4,
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_polymorphic_pointer_and_array_ownership_of_local_finalizers() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  type(payload_t) :: source
  class(*), pointer :: pointed
  class(*), allocatable :: assigned(:), sourced(:)
  allocate(payload_t :: pointed)
  assigned = [source]
  allocate(sourced, source=[source])
contains
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("cannot preserve its local FINAL procedure"))
                .count(),
            3,
            "{:?}",
            errs
        );
    }

    #[test]
    fn ignores_user_defined_move_alloc_procedures() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  type(payload_t) :: source
  class(*), allocatable :: target
  call move_alloc(source, target)
contains
  subroutine move_alloc(from, to)
    type(payload_t), intent(in) :: from
    class(*), allocatable, intent(out) :: to
  end subroutine
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("cannot preserve its local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_constructor_assignment_to_polymorphic_components() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  type :: holder_t
    class(*), allocatable :: value
  end type
  type(holder_t) :: direct
  class(holder_t), allocatable :: polymorphic_holder
  allocate(polymorphic_holder)
  direct%value = payload_t()
  polymorphic_holder%value = payload_t()
contains
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("cannot preserve its local FINAL procedure"))
                .count(),
            2,
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_defined_assignment_from_local_finalizable_source() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  interface assignment(=)
    subroutine assign_payload(lhs, rhs)
      class(*), allocatable, intent(out) :: lhs
      type(payload_t), intent(in) :: rhs
    end subroutine
  end interface
  type(payload_t) :: source
  class(*), allocatable :: assigned
  assigned = source
contains
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("cannot preserve its local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_array_defined_assignment_without_exact_lowering_match() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  interface assignment(=)
    subroutine assign_payload(lhs, rhs)
      class(*), allocatable, intent(out) :: lhs(:)
      type(payload_t), intent(in) :: rhs(:)
    end subroutine
  end interface
  type(payload_t) :: source(1)
  class(*), allocatable :: assigned(:)
  assigned = source
contains
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("cannot preserve its local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_static_ownership_of_local_finalizable_type() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  type(payload_t), allocatable :: concrete
  allocate(concrete)
  deallocate(concrete)
contains
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("cannot preserve its local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_context_free_polymorphic_ownership() {
        let errs = errors_from(
            "\
module final_m
  implicit none
  type :: module_t
  contains
    final :: finish_module
  end type
contains
  subroutine finish_module(item)
    type(module_t) :: item
  end subroutine
end module
program p
  use final_m, only: module_t
  implicit none
  type :: local_t
    integer, allocatable :: values(:)
  end type
  class(*), allocatable :: local_value, module_value
  allocate(local_t :: local_value)
  allocate(module_t :: module_value)
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("cannot preserve its local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_local_recursive_ownership_with_finalizer() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: node_t
    type(node_t), allocatable :: child
  contains
    final :: finish
  end type
contains
  subroutine finish(item)
    type(node_t) :: item
  end subroutine
end program
",
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("locally declared derived type") && err.contains("local FINAL binding")
            }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_local_allocatable_component_with_local_finalizer() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
    integer :: value = 0
  contains
    final :: finish
  end type
  type :: holder_t
    type(payload_t), allocatable :: payload
  end type
contains
  subroutine finish(item)
    type(payload_t) :: item
  end subroutine
end program
",
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("holder_t")
                    && err.contains("allocatable ownership")
                    && err.contains("local FINAL binding")
            }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_module_owned_recursive_dynamic_ownership() {
        let errs = errors_from(
            "\
module ownership_mod
  implicit none
  type :: dynamic_node_t
    class(*), allocatable :: child
  end type
  type :: final_node_t
    type(final_node_t), allocatable :: child
  contains
    final :: finish
  end type
contains
  subroutine finish(item)
    type(final_node_t) :: item
  end subroutine
end module
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("locally declared derived type")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_local_finalizer_host_automatic_capture() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value()
    integer :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
    calls = 1
  contains
    subroutine finish(item)
      type(payload_t) :: item
      calls = calls + item%marker
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| { err.contains("local FINAL procedure") && err.contains("calls") }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_local_finalizer_host_dummy_capture() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value(calls)
    integer, intent(inout) :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      calls = calls + item%marker
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| { err.contains("local FINAL procedure") && err.contains("calls") }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_local_finalizer_ancestor_host_capture() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine outer()
    integer :: calls
  contains
    subroutine make_value()
      type :: payload_t
        integer :: marker
      contains
        final :: finish
      end type
      type(payload_t) :: value
    contains
      subroutine finish(item)
        type(payload_t) :: item
        calls = calls + item%marker
      end subroutine
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| { err.contains("local FINAL procedure") && err.contains("calls") }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_transitive_local_finalizer_host_capture() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value()
    integer :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      call record(item%marker)
    end subroutine
    subroutine record(marker)
      integer, intent(in) :: marker
      calls = calls + marker
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| { err.contains("local FINAL procedure") && err.contains("calls") }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_context_free_local_finalizers() {
        let errs = errors_from(
            "\
module state_mod
  implicit none
  integer :: module_calls
contains
  subroutine make_value()
    integer, parameter :: increment = 1
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      module_calls = module_calls + item%marker + increment
    end subroutine
  end subroutine
end module
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_local_finalizer_construct_name_shadowing() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value()
    integer :: i
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      integer :: total
      total = sum([(i, i = 1, 2)]) + item%marker
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_local_finalizer_shadowed_sibling_name() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value()
    integer :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      integer :: record(1)
      item%marker = record(1)
    end subroutine
    subroutine record()
      calls = calls + 1
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_local_finalizer_block_use_shadowing() {
        let errs = errors_from(
            "\
module state_mod
  implicit none
  integer :: module_calls
end module

program p
  implicit none
contains
  subroutine make_value()
    integer :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      block
        use state_mod, only: calls => module_calls
        calls = calls + item%marker
      end block
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn accepts_local_finalizer_do_concurrent_local_shadowing() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value()
    integer :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      integer :: i
      do concurrent (i = 1:1) local(calls)
        calls = item%marker
      end do
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("local FINAL procedure")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_local_finalizer_do_concurrent_local_init_capture() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value()
    integer :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      integer :: i
      do concurrent (i = 1:1) local_init(calls)
        calls = calls + item%marker
      end do
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| { err.contains("local FINAL procedure") && err.contains("calls") }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn rejects_local_finalizer_saved_host_capture() {
        let errs = errors_from(
            "\
program p
  implicit none
contains
  subroutine make_value()
    integer, save :: calls
    type :: payload_t
      integer :: marker
    contains
      final :: finish
    end type
    type(payload_t) :: value
  contains
    subroutine finish(item)
      type(payload_t) :: item
      calls = calls + item%marker
    end subroutine
  end subroutine
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| { err.contains("local FINAL procedure") && err.contains("calls") }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn implicit_none_rejects_unknown_namelist_group_in_nml_control() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: x
  namelist /expected/ x
  read(*, nml=missing)
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("missing")), "{:?}", errs);
    }

    #[test]
    fn implicit_none_accepts_host_namelist_group_in_contained_proc() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: x
  namelist /act_cli/ x
contains
  subroutine parse()
    write(*, nml=act_cli)
  end subroutine
end program
",
        );
        assert!(!errs.iter().any(|e| e.contains("act_cli")), "{:?}", errs);
    }

    // ---- F2023 enumeration types ----

    const ENUM_PRELUDE: &str = "\
program p
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  enumeration type :: fruit
    enumerator :: apple, pear
  end enumeration type
  type(color) :: c
  type(fruit) :: f
  integer :: n
";

    fn enum_errors(body: &str) -> Vec<String> {
        errors_from(&format!("{}{}\nend program\n", ENUM_PRELUDE, body))
    }

    #[test]
    fn enum_assign_integer_rejected() {
        let errs = enum_errors("  c = 1");
        assert!(
            errs.iter().any(|e| e.contains("use the constructor")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_assign_cross_type_rejected() {
        let errs = enum_errors("  f = red");
        assert!(
            errs.iter().any(|e| e.contains("enumeration type 'color'")
                && e.contains("enumeration type 'fruit'")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_assign_to_integer_rejected() {
        let errs = enum_errors("  c = red\n  n = c");
        assert!(
            errs.iter().any(|e| e.contains("convert with INT(v)")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_arithmetic_rejected() {
        let errs = enum_errors("  c = red\n  n = int(c) + 1\n  c = c + 1");
        assert!(
            errs.iter()
                .any(|e| e.contains("operator '+' is not defined for enumeration type 'color'")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_same_type_relational_allowed() {
        let errs = enum_errors(
            "  c = red\n  if (c == red) n = 1\n  if (c < blue) n = 2\n  if (c >= green) n = 3",
        );
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn enum_cross_type_relational_rejected() {
        let errs = enum_errors("  if (red == apple) n = 1");
        assert!(
            errs.iter().any(|e| e.contains("same enumeration type")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_integer_relational_rejected() {
        let errs = enum_errors("  c = red\n  if (c == 1) n = 1");
        assert!(
            errs.iter()
                .any(|e| e.contains("cannot compare enumeration type")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_constructor_range_checked() {
        let errs = enum_errors("  c = color(0)\n  c = color(4)\n  c = color(3)");
        assert_eq!(
            errs.iter().filter(|e| e.contains("out of range")).count(),
            2,
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_list_directed_print_rejected() {
        let errs = enum_errors("  c = red\n  print *, c\n  print *, int(c)");
        assert_eq!(
            errs.iter()
                .filter(|e| e.contains("list-directed output"))
                .count(),
            1,
            "{:?}",
            errs
        );
    }

    #[test]
    fn enum_call_argument_mismatch_rejected() {
        let errs = errors_from(
            "\
program p
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = red
  call takes_int(c)
  call takes_color(2)
  call takes_color(c)
contains
  subroutine takes_int(n)
    integer, intent(in) :: n
    print *, n
  end subroutine
  subroutine takes_color(x)
    type(color), intent(in) :: x
    integer :: m
    m = int(x)
  end subroutine
end program
",
        );
        assert!(
            errs.iter().any(|e| e.contains("non-enumeration dummy 'n'")),
            "{:?}",
            errs
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("dummy argument 'x' has enumeration type 'color'")),
            "{:?}",
            errs
        );
        assert_eq!(errs.len(), 2, "{:?}", errs);
    }

    #[test]
    fn enumeration_type_requires_f2023() {
        let src = "\
program p
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
end program
";
        let errs = errors_with_std(src, FortranStandard::F2018);
        assert!(
            errs.iter()
                .any(|e| e.contains("ENUMERATION TYPE requires --std=F2023")),
            "{:?}",
            errs
        );
        let errs = errors_with_std(src, FortranStandard::F2023);
        assert!(errs.is_empty(), "{:?}", errs);
    }

    // ---- Character VALUE dummies ----

    #[test]
    fn char_value_dummy_rejected_without_bind_c() {
        let errs = errors_from(
            "\
subroutine foo(c)
  character(1), value :: c
end subroutine
",
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("VALUE attribute on CHARACTER")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn char_value_dummy_allowed_in_bind_c() {
        let errs = errors_from(
            "\
subroutine foo(c) bind(c, name='foo')
  use iso_c_binding
  character(kind=c_char), value :: c
end subroutine
",
        );
        assert!(
            !errs
                .iter()
                .any(|e| e.contains("VALUE attribute on CHARACTER")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn char_value_dummy_allowed_in_bind_c_interface_body() {
        let errs = errors_from(
            "\
program p
  use iso_c_binding
  interface
    function check_char(ch) result(rc) bind(c, name='check_char')
      import :: c_char, c_int
      character(kind=c_char), value :: ch
      integer(c_int) :: rc
    end function
  end interface
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|e| e.contains("VALUE attribute on CHARACTER")),
            "{:?}",
            errs
        );
    }

    // ---- Intent enforcement ----

    #[test]
    fn assign_to_intent_in_errors() {
        let errs = errors_from(
            "\
subroutine foo(x)
  real, intent(in) :: x
  x = 1.0
end subroutine
",
        );
        assert!(errs.iter().any(|e| e.contains("intent(in)")));
    }

    #[test]
    fn assign_to_intent_inout_ok() {
        let errs = errors_from(
            "\
subroutine foo(x)
  real, intent(inout) :: x
  x = 1.0
end subroutine
",
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn assign_through_intent_in_pointer_target_ok() {
        let errs = errors_from(
            "\
module m
  type :: t
    integer :: x
  end type
contains
  subroutine foo(p)
    type(t), pointer, intent(in) :: p
    p%x = 1
  end subroutine
end module
",
        );
        assert!(!errs.iter().any(|e| e.contains("intent(in)")), "{:?}", errs);
    }

    #[test]
    fn assign_to_parameter_errors() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer, parameter :: n = 10
  n = 20
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("named constant")));
    }

    // ---- Allocatable / pointer ----

    #[test]
    fn allocate_non_allocatable_errors() {
        let errs = errors_from(
            "\
program test
  implicit none
  real :: x(10)
  allocate(x(20))
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("allocatable or pointer")));
    }

    #[test]
    fn allocate_allocatable_ok() {
        let errs = errors_from(
            "\
program test
  implicit none
  real, allocatable :: x(:)
  allocate(x(10))
end program
",
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn allocatable_and_pointer_forbidden() {
        let errs = errors_from(
            "\
program test
  implicit none
  real, allocatable, pointer :: x
end program
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("both allocatable and pointer")));
    }

    #[test]
    fn parameter_allocatable_forbidden() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer, parameter, allocatable :: x = 10
end program
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("parameter") && e.contains("allocatable")));
    }

    // ---- Pointer assignment ----

    #[test]
    fn pointer_assignment_non_pointer_errors() {
        let errs = errors_from(
            "\
program test
  implicit none
  real :: x
  real, target :: y
  x => y
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("pointer attribute")));
    }

    #[test]
    fn pointer_assignment_non_target_errors() {
        let errs = errors_from(
            "\
program test
  implicit none
  real, pointer :: p
  real :: x
  p => x
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("target or pointer")));
    }

    #[test]
    fn pointer_assignment_ok() {
        let errs = errors_from(
            "\
program test
  implicit none
  real, pointer :: p
  real, target :: x
  p => x
end program
",
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn pointer_assignment_from_allocatable_component_element_ok() {
        let errs = errors_from(
            "\
program test
  implicit none
  type :: node
    type(node), allocatable :: children(:)
  end type
  type(node), target :: root
  type(node), pointer :: p
  allocate(root%children(1))
  p => root%children(1)
end program
",
        );
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    // ---- Pure constraints ----

    #[test]
    fn io_in_pure_errors() {
        let errs = errors_from(
            "\
pure subroutine foo(x)
  real, intent(in) :: x
  print *, x
end subroutine
",
        );
        assert!(errs.iter().any(|e| e.contains("I/O") && e.contains("pure")));
    }

    #[test]
    fn internal_write_in_pure_ok() {
        let errs = errors_from(
            "\
pure function fmt_value(x) result(str)
  integer, intent(in) :: x
  character(16) :: str
  write(str, '(i0)') x
end function
",
        );
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn stop_in_pure_errors() {
        let errs = errors_from(
            "\
pure function bar(x) result(y)
  real, intent(in) :: x
  real :: y
  y = x
  stop
end function
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("STOP") && e.contains("pure")));
    }

    #[test]
    fn save_in_pure_errors() {
        let errs = errors_from(
            "\
pure subroutine foo(x)
  real, intent(in) :: x
  real, save :: counter
end subroutine
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("SAVE") && e.contains("pure")));
    }

    #[test]
    fn pure_without_violations_ok() {
        let errs = errors_from(
            "\
pure function square(x) result(y)
  real, intent(in) :: x
  real :: y
  y = x * x
end function
",
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn pure_write_to_module_variable_errors() {
        let errs = errors_from(
            "\
module m
  integer :: counter = 0
contains
  pure integer function writes_counter() result(r)
    counter = 99
    r = counter
  end function
end module
",
        );
        assert!(
            errs.iter().any(|e| e.contains("counter")
                && e.contains("pure")
                && e.contains("host or use association")),
            "expected pure+module-write error, got {:?}",
            errs,
        );
    }

    #[test]
    fn pure_read_of_module_variable_ok() {
        // F2018 15.7 permits a pure procedure to *reference* a
        // variable accessed by use association; only definition
        // is forbidden.  reads_counter is a legal pure function.
        let errs = errors_from(
            "\
module m
  integer :: counter = 0
contains
  pure integer function reads_counter() result(r)
    r = counter
  end function
end module
",
        );
        assert!(
            errs.is_empty(),
            "pure read of module variable should be legal, got {:?}",
            errs
        );
    }

    #[test]
    fn pure_write_to_host_variable_errors() {
        let errs = errors_from(
            "\
program p
  integer :: host_var
  host_var = 0
  call helper()
contains
  pure subroutine helper()
    host_var = 42
  end subroutine
end program
",
        );
        assert!(
            errs.iter().any(|e| e.contains("host_var")
                && e.contains("pure")
                && e.contains("host or use association")),
            "expected pure+host-write error, got {:?}",
            errs,
        );
    }

    #[test]
    fn pure_pointer_reassoc_of_module_pointer_errors() {
        let errs = errors_from(
            "\
module m
  integer, pointer :: module_p
contains
  pure subroutine reassoc(t)
    integer, target, intent(in) :: t
    module_p => t
  end subroutine
end module
",
        );
        assert!(
            errs.iter().any(|e| e.contains("module_p")
                && e.contains("pure")
                && e.contains("pointer assignment")),
            "expected pure+module-pointer error, got {:?}",
            errs,
        );
    }

    #[test]
    fn pure_local_pointer_reassoc_ok() {
        // Associating a LOCAL pointer with a module TARGET is
        // legal — `q => counter` does not modify `counter`.
        let errs = errors_from(
            "\
module m
  integer, target :: counter = 0
contains
  pure integer function associates_counter() result(r)
    integer, pointer :: q
    q => counter
    r = 0
  end function
end module
",
        );
        assert!(
            errs.is_empty(),
            "pure local pointer reassoc should be legal, got {:?}",
            errs
        );
    }

    #[test]
    fn pure_intent_out_dummy_ok() {
        let errs = errors_from(
            "\
pure subroutine zero_it(x)
  integer, intent(out) :: x
  x = 0
end subroutine
",
        );
        assert!(
            errs.is_empty(),
            "pure write to intent(out) dummy should be legal, got {:?}",
            errs
        );
    }

    // ---- Deferred length character ----

    #[test]
    fn deferred_len_without_allocatable_errors() {
        let errs = errors_from(
            "\
program test
  implicit none
  character(len=:) :: s
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("deferred-length")));
    }

    #[test]
    fn deferred_len_with_allocatable_ok() {
        let errs = errors_from(
            "\
program test
  implicit none
  character(len=:), allocatable :: s
end program
",
        );
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
        use crate::lexer::{Position, Span};
        let st = SymbolTable::new();
        let mut ctx = Ctx::new(&st, None, false, false);
        let span = Span {
            file_id: 0,
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        };

        register_label(&mut ctx, 10, span);
        register_label(&mut ctx, 10, span); // duplicate
        assert!(ctx.diags.iter().any(|d| d.msg.contains("duplicate label")));
    }

    // ---- Valid code produces no errors ----

    #[test]
    fn clean_program_no_errors() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, n
  real :: x
  n = 10
  do i = 1, n
    x = real(i) * 2.0
  end do
end program
",
        );
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn module_with_subroutine_no_errors() {
        let errs = errors_from(
            "\
module mymod
  implicit none
  integer :: shared
contains
  subroutine update(val)
    integer, intent(in) :: val
    shared = val
  end subroutine
end module
",
        );
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn module_parameter_visible_in_contained_subroutine() {
        let errs = errors_from(
            "\
module m
  use iso_c_binding, only: c_int
  implicit none
  private
  public :: s
  integer, parameter :: color_red = 31
contains
  subroutine s()
    use iso_c_binding, only: c_int
    print *, color_red
  end subroutine
end module
",
        );
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    // ---- Defined operator validation ----
    // Note: the parser doesn't yet support interface blocks in the module
    // specification section (they must appear as top-level units or in
    // CONTAINS). These tests use the validation API directly.

    #[test]
    fn operator_interface_subroutine_errors() {
        // Parse a top-level interface block with operator name.
        let errs = errors_from(
            "\
interface operator(+)
  subroutine bad_add(a, b)
    integer, intent(in) :: a, b
  end subroutine
end interface
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("functions, not subroutines")));
    }

    #[test]
    fn operator_interface_wrong_arg_count() {
        let errs = errors_from(
            "\
interface operator(+)
  function add3(a, b, c) result(r)
    integer, intent(in) :: a, b, c
    integer :: r
  end function
end interface
",
        );
        assert!(errs.iter().any(|e| e.contains("1 or 2 arguments")));
    }

    #[test]
    fn operator_interface_valid_binary() {
        let errs = errors_from(
            "\
interface operator(+)
  function add_vec(a, b) result(c)
    integer, intent(in) :: a, b
    integer :: c
  end function
end interface
",
        );
        assert!(errs.is_empty(), "unexpected errors: {:?}", errs);
    }

    #[test]
    fn assignment_interface_function_errors() {
        let errs = errors_from(
            "\
interface assignment(=)
  function bad_assign(a, b) result(c)
    integer, intent(in) :: a, b
    integer :: c
  end function
end interface
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("subroutines, not functions")));
    }

    #[test]
    fn assignment_interface_wrong_arg_count() {
        let errs = errors_from(
            "\
interface assignment(=)
  subroutine bad_assign(a, b, c)
    integer, intent(inout) :: a
    integer, intent(in) :: b, c
  end subroutine
end interface
",
        );
        assert!(errs.iter().any(|e| e.contains("2 arguments")));
    }

    // ---- Derived type validation ----

    #[test]
    fn deferred_in_non_abstract_errors() {
        let errs = errors_from(
            "\
module m
  implicit none
  type :: shape
  contains
    procedure, deferred :: area
  end type
end module
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("DEFERRED") && e.contains("not ABSTRACT")));
    }

    #[test]
    fn deferred_in_abstract_ok() {
        let errs = errors_from(
            "\
module m
  implicit none
  type, abstract :: shape
  contains
    procedure, deferred :: area
  end type
end module
",
        );
        // No error for deferred in abstract type (the "must specify interface"
        // error is expected since our parser stores binding as None for simple
        // deferred procedures — that's a parser representation issue).
        assert!(!errs.iter().any(|e| e.contains("not ABSTRACT")));
    }

    #[test]
    fn pass_and_nopass_together_errors() {
        let errs = errors_from(
            "\
module m
  implicit none
  type :: thing
  contains
    procedure, pass, nopass :: method
  end type
end module
",
        );
        assert!(errs.iter().any(|e| e.contains("both PASS and NOPASS")));
    }

    // ---- Standard conformance (--std=) ----

    #[test]
    fn do_concurrent_requires_f2008() {
        let errs = errors_with_std(
            "\
program test
  implicit none
  integer :: i
  do concurrent (i = 1:10)
  end do
end program
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("DO CONCURRENT") && e.contains("F2008")));
    }

    #[test]
    fn do_concurrent_local_referenced_in_header_rejected() {
        // F2023 C1133: reading a LOCAL variable in the concurrent-header.
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, j
  do concurrent (i = j:10) local(j)
  end do
end program
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("LOCAL locality") && e.contains("C1133")));
    }

    #[test]
    fn do_concurrent_local_not_in_header_ok() {
        // A LOCAL variable not referenced in the header is legal.
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, j
  do concurrent (i = 1:10) local(j)
    j = i
  end do
end program
",
        );
        assert!(!errs.iter().any(|e| e.contains("C1133")));
    }

    #[test]
    fn do_concurrent_local_init_in_header_ok() {
        // LOCAL_INIT is initialized from the outer scope, so referencing
        // it in the header is allowed — C1133 covers LOCAL only.
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, j
  do concurrent (i = j:10) local_init(j)
  end do
end program
",
        );
        assert!(!errs.iter().any(|e| e.contains("C1133")));
    }

    #[test]
    fn array_conditional_rhs_accepted() {
        // F2023: an array-valued conditional as an assignment RHS lowers
        // via a per-arm branch, so it is accepted.
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: a(3), b(3), x(3)
  logical :: c
  c = .true.
  x = (c ? a : b)
end program
",
        );
        assert!(
            !errs.iter().any(|e| e.contains("array-valued arms")),
            "array conditional RHS should be accepted, got {errs:?}"
        );
    }

    #[test]
    fn array_conditional_in_binop_rejected() {
        // Array conditional buried in a larger expression has no descriptor
        // lowering and stays rejected.
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: a(3), b(3), x(3)
  logical :: c
  c = .true.
  x = (c ? a : b) + a
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("array-valued arms")));
    }

    #[test]
    fn array_conditional_in_print_rejected() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: a(3), b(3)
  logical :: c
  c = .true.
  print *, (c ? a : b)
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("array-valued arms")));
    }

    #[test]
    fn nil_arm_to_required_dummy_rejected() {
        // F2023 C1525: .NIL. against a non-optional dummy.
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: a
  a = 3
  call req((a > 0 ? a : .nil.))
contains
  subroutine req(x)
    integer, intent(in) :: x
  end subroutine req
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("C1525")));
    }

    #[test]
    fn nil_arm_to_optional_dummy_accepted() {
        let errs = errors_from(
            "\
program test
  implicit none
  call maybe((.true. ? 7 : .nil.))
contains
  subroutine maybe(o)
    integer, intent(in), optional :: o
  end subroutine maybe
end program
",
        );
        assert!(!errs.iter().any(|e| e.contains("C1525")));
    }

    #[test]
    fn conditional_arg_in_function_reference_accepted() {
        // F2023: a conditional actual argument in a function reference is
        // lowered like the CALL path (association selection), so it is no
        // longer rejected.
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: a, r
  a = 3
  r = twice((a > 0 ? a : 1))
contains
  integer function twice(x)
    integer, intent(in) :: x
    twice = 2 * x
  end function twice
end program
",
        );
        assert!(
            !errs.iter().any(|e| e.contains("only supported in CALL")),
            "conditional arg in a function reference should be accepted, got {errs:?}"
        );
    }

    #[test]
    fn do_concurrent_ok_with_f2008() {
        let errs = errors_with_std(
            "\
program test
  implicit none
  integer :: i
  do concurrent (i = 1:10)
  end do
end program
",
            FortranStandard::F2008,
        );
        assert!(!errs.iter().any(|e| e.contains("DO CONCURRENT")));
    }

    #[test]
    fn error_stop_requires_f2008() {
        let errs = errors_with_std(
            "\
program test
  implicit none
  error stop
end program
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("ERROR STOP") && e.contains("F2008")));
    }

    #[test]
    fn do_loop_zero_step_is_rejected() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i
  do i = 1, 10, 0
  end do
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("DO step must not be zero")));
    }

    #[test]
    fn select_case_rejects_multiple_default_arms() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: x
  x = 7
  select case (x)
  case default
    print *, 0
  case default
    print *, 9
  end select
end program
",
        );
        assert!(errs.iter().any(|e| e.contains("multiple CASE DEFAULT")));
    }

    #[test]
    fn select_case_rejects_overlapping_integer_ranges() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: x
  x = 7
  select case (x)
  case (1:10)
    print *, 1
  case (5:8)
    print *, 2
  end select
end program
",
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("SELECT CASE selectors must be mutually exclusive")));
    }

    #[test]
    fn block_construct_requires_f2008() {
        let errs = errors_with_std(
            "\
program test
  implicit none
  block
    x = 1
  end block
end program
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("BLOCK") && e.contains("F2008")));
    }

    #[test]
    fn associate_requires_f2003() {
        let errs = errors_with_std(
            "\
program test
  implicit none
  integer :: n
  n = 10
  associate (m => n)
  end associate
end program
",
            FortranStandard::F95,
        );
        assert!(errs
            .iter()
            .any(|e| e.contains("ASSOCIATE") && e.contains("F2003")));
    }

    #[test]
    fn no_std_violations_when_unset() {
        // With no --std= set, everything is allowed.
        let errs = errors_from(
            "\
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
        assert!(errs
            .iter()
            .any(|e| e.contains("IMPURE") && e.contains("F2008")));
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
        let diags =
            validate_file_with_std(&[unit], &SymbolTable::new(), Some(FortranStandard::F95));
        let errs: Vec<_> = diags
            .into_iter()
            .filter(|d| d.kind == DiagKind::Error)
            .map(|d| d.msg)
            .collect();
        assert!(errs
            .iter()
            .any(|e| e.contains("SUBMODULE") && e.contains("F2008")));
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
        assert!(errs
            .iter()
            .any(|e| e.contains("MOVE_ALLOC") && e.contains("F2003")));
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
