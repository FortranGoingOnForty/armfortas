//! Semantic validation — checks that go beyond type checking.
//!
//! Allocatable/pointer semantics, intent enforcement, pure/elemental
//! constraints, label validation, and standard conformance. Runs after
//! symbol resolution (resolve.rs) and type checking (types.rs).

use crate::sema::symtab::*;
use crate::sema::types::{
    binary_op_result_type, expr_type, intrinsic_result_type, type_info_to_fortran_type,
    unary_op_result_type,
};

use super::allocatable::{
    allocate_item_needs_explicit_shape, expr_selects_component, leaf_field_layout,
    validate_allocatable_item,
};
use super::pointer::validate_pointer_assignment;
use super::procedure::{
    validate_procedure_dummy_actual, validate_procedure_pointer_initializer,
    validate_procedure_pointer_interface,
};
use super::pure_elemental::{
    check_pure_stmt_expr_calls, reject_pure_nonlocal_definition, validate_elemental_args,
    validate_pure_call,
};
use crate::ast::decl::{Attribute, Decl, OnlyItem, SpannedDecl, TypeAttr, TypeSpec, UseNature};
use crate::ast::expr::{AcValue, Argument, Expr, SectionSubscript, SpannedExpr};
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
    /// Derived types inherited by ASSOCIATE names from their selectors.
    /// ASSOCIATE is not a symbol-table scope, but component validation must
    /// still know the alias's type after lexical shadowing takes effect.
    associate_type_frames: Vec<HashMap<String, TypeInfo>>,
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
    /// One ambiguity in a host scope can be referenced by several contained
    /// procedures. Report it once at the first source reference.
    reported_use_ambiguities: HashSet<(ScopeId, String)>,
    /// A malformed merged generic can remain visible through host association
    /// in several nested scopes. Key the exact cross-owner pair so the
    /// declaration error is emitted once instead of once per reference.
    reported_indistinguishable_generics: HashSet<(String, ScopeId, String, ScopeId, String)>,
    /// A restricted IMPORT policy can make the same host entity appear in
    /// several declaration expressions. Diagnose the first reference only.
    reported_inaccessible_host_entities: HashSet<(ScopeId, String)>,
    /// BLOCK constructs do not have symbol-table scope IDs, so their
    /// ambiguity diagnostics are keyed by the construct's source position.
    reported_block_use_ambiguities: HashSet<(u32, u32, u32, String)>,
    /// BLOCK and ASSOCIATE constructs are outside the symbol-table scope
    /// graph. Preserve their interleaved lexical order so local bindings and
    /// BLOCK USE associations shadow outer names correctly.
    ambiguity_lexical_frames: Vec<AmbiguityLexicalFrame>,
}

#[derive(Clone, Default)]
struct BlockBindingAttrs {
    intent_in: bool,
    parameter: bool,
    allocatable: bool,
    pointer: bool,
    type_info: Option<TypeInfo>,
    rank: Option<usize>,
}

#[derive(Clone)]
struct BlockImportControl {
    policy: HostAssociationPolicy,
    protection: HostImportProtection,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LexicalHostVisibility {
    Visible,
    Hidden,
    Missing,
}

enum AmbiguityLexicalFrame {
    Block {
        span: Span,
        scope_id: Option<ScopeId>,
        uses: Vec<SpannedDecl>,
        bindings: HashSet<String>,
        named_types: HashSet<String>,
        import_control: Box<BlockImportControl>,
    },
    Associate {
        bindings: HashSet<String>,
    },
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
            associate_type_frames: Vec::new(),
            block_decl_frames: Vec::new(),
            warn_pedantic,
            warn_deprecated,
            current_args: HashSet::new(),
            in_call_arg: false,
            allow_array_cond_rhs: false,
            in_bind_c_unit: false,
            finalizer_capture_host_scopes: HashSet::new(),
            reported_finalizer_captures: HashSet::new(),
            reported_use_ambiguities: HashSet::new(),
            reported_indistinguishable_generics: HashSet::new(),
            reported_inaccessible_host_entities: HashSet::new(),
            reported_block_use_ambiguities: HashSet::new(),
            ambiguity_lexical_frames: Vec::new(),
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
    pub(super) fn is_associate_name(&self, name: &str) -> bool {
        let key = name.to_lowercase();
        self.associate_frames
            .iter()
            .any(|frame| frame.contains(&key))
    }

    pub(super) fn is_block_local_name(&self, name: &str) -> bool {
        self.block_binding_attrs(name).is_some()
    }

    fn block_binding_attrs(&self, name: &str) -> Option<&BlockBindingAttrs> {
        let key = name.to_lowercase();
        self.block_decl_frames
            .iter()
            .rev()
            .find_map(|frame| frame.get(&key))
    }

    fn associate_binding_type_info(&self, name: &str) -> Option<&TypeInfo> {
        let key = name.to_lowercase();
        self.associate_type_frames
            .iter()
            .rev()
            .find_map(|frame| frame.get(&key))
    }

    fn block_array_element_type_info(&self, name: &str) -> Option<&TypeInfo> {
        let binding = self.block_binding_attrs(name)?;
        binding.rank.filter(|rank| *rank > 0)?;
        binding.type_info.as_ref()
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

    pub(super) fn lookup_lexical(&self, name: &str) -> Option<&'a Symbol> {
        let key = name.to_lowercase();
        for frame in self.ambiguity_lexical_frames.iter().rev() {
            match frame {
                AmbiguityLexicalFrame::Associate { bindings } => {
                    if bindings.contains(&key) {
                        return None;
                    }
                }
                AmbiguityLexicalFrame::Block {
                    scope_id,
                    uses,
                    bindings,
                    import_control,
                    ..
                } => {
                    if let Some(symbol) = scope_id.and_then(|scope_id| {
                        self.st
                            .scope(scope_id)
                            .symbols
                            .get(&key)
                            .or_else(|| self.st.named_interface_symbol_in_scope(scope_id, &key))
                    }) {
                        return Some(symbol);
                    }
                    if bindings.contains(&key) {
                        return None;
                    }
                    let associations = block_use_associations_for_name(self.st, uses, &key);
                    if let Some(symbol) = self.st.lookup_from_use_associations(&associations, &key)
                    {
                        return Some(symbol);
                    }
                    if !import_control.policy.allows(&key) {
                        return None;
                    }
                }
            }
        }
        self.lookup(name)
    }

    fn lookup_lexical_named_interfaces(&self, name: &str) -> Vec<&'a Symbol> {
        let key = name.to_lowercase();
        for frame in self.ambiguity_lexical_frames.iter().rev() {
            match frame {
                AmbiguityLexicalFrame::Associate { bindings } => {
                    if bindings.contains(&key) {
                        return Vec::new();
                    }
                }
                AmbiguityLexicalFrame::Block {
                    scope_id,
                    uses,
                    bindings,
                    import_control,
                    ..
                } => {
                    if let Some(symbol) = scope_id.and_then(|scope_id| {
                        self.st
                            .named_interface_facet_symbol_in_scope(scope_id, &key)
                    }) {
                        return vec![symbol];
                    }
                    if bindings.contains(&key) {
                        return Vec::new();
                    }
                    let associations = block_use_associations_for_name(self.st, uses, &key);
                    let symbols = self
                        .st
                        .named_interface_symbols_from_use_associations(&associations, &key);
                    if !symbols.is_empty() {
                        return symbols;
                    }
                    if !import_control.policy.allows(&key) {
                        return Vec::new();
                    }
                }
            }
        }
        self.st.named_interface_symbols_in(self.scope_id, &key)
    }

    fn lexical_scope_id(&self) -> ScopeId {
        self.ambiguity_lexical_frames
            .iter()
            .rev()
            .find_map(|frame| match frame {
                AmbiguityLexicalFrame::Block { scope_id, .. } => *scope_id,
                AmbiguityLexicalFrame::Associate { .. } => None,
            })
            .unwrap_or(self.scope_id)
    }

    fn lexical_local_named_type(&self, name: &str) -> Option<bool> {
        let key = name.to_lowercase();
        for frame in self.ambiguity_lexical_frames.iter().rev() {
            match frame {
                AmbiguityLexicalFrame::Associate { bindings } => {
                    if bindings.contains(&key) {
                        return Some(false);
                    }
                }
                AmbiguityLexicalFrame::Block {
                    bindings,
                    named_types,
                    ..
                } => {
                    if named_types.contains(&key) {
                        return Some(true);
                    }
                    if bindings.contains(&key) {
                        return Some(false);
                    }
                }
            }
        }
        None
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

fn default_real_kind(kind: Option<u8>) -> u8 {
    kind.unwrap_or_else(crate::driver::defaults::default_real_kind)
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

fn validate_supported_character_type_spec(ctx: &mut Ctx<'_>, span: Span, type_spec: &TypeSpec) {
    let TypeSpec::Character(Some(selector)) = type_spec else {
        return;
    };
    let Some(kind_expr) = selector.kind.as_ref() else {
        return;
    };
    let Ok(Some(kind)) = eval_const_int_expr_checked(ctx, kind_expr) else {
        return;
    };
    if kind.value != 1 {
        ctx.error(
            span,
            format!(
                "CHARACTER(kind={}) data is not supported: the backend and runtime support only CHARACTER(kind=1)",
                kind.value
            ),
        );
    }
}

fn symbol_is_named_type(symbol: &Symbol) -> bool {
    matches!(
        symbol.kind,
        SymbolKind::DerivedType | SymbolKind::EnumerationType
    )
}

/// Validate a source-level reference to a named derived/enumeration type.
///
/// Type layouts have a wider lifetime than source visibility: private
/// layouts must remain available after `.amod` reconstruction so descendant
/// submodules and public layouts with private component types can be lowered.
/// Therefore a registry hit is never proof that a source name is accessible.
/// All source spellings must resolve through the symbol table, whose ordinary
/// USE lookup filters non-exported symbols while submodule host association
/// deliberately retains them.
fn validate_visible_named_type(ctx: &mut Ctx<'_>, span: Span, name: &str) {
    match ctx.lexical_local_named_type(name) {
        Some(true) => return,
        Some(false) => {
            ctx.error(
                span,
                format!("'{}' does not name a derived type in this scope", name),
            );
            return;
        }
        None => {}
    }

    match ctx.lookup_lexical(name) {
        Some(symbol) if symbol_is_named_type(symbol) => {}
        Some(_) => ctx.error(
            span,
            format!("'{}' does not name a derived type in this scope", name),
        ),
        None => ctx.error(
            span,
            format!("derived type '{}' is not accessible in this scope", name),
        ),
    }
}

fn validate_visible_type_spec(ctx: &mut Ctx<'_>, span: Span, type_spec: &TypeSpec) {
    if let TypeSpec::Type(name) | TypeSpec::Class(name) = type_spec {
        validate_visible_named_type(ctx, span, name);
    }
}

fn type_decl_encodes_procedure_interface(attrs: &[Attribute], type_spec: &TypeSpec) -> bool {
    matches!(type_spec, TypeSpec::Type(_))
        && attrs
            .iter()
            .any(|attr| matches!(attr, Attribute::Procedure))
}

/// Array-constructor and SELECT TYPE guards retain their type-spec as source
/// text. Intrinsic specs parse directly; a bare identifier is a derived or
/// enumeration type name and must pass the same visibility check as TYPE(t).
fn validate_visible_raw_type_spec(ctx: &mut Ctx<'_>, span: Span, raw: &str) {
    if let Ok(tokens) =
        crate::lexer::tokenize(raw, span.file_id, crate::lexer::SourceForm::FreeForm)
    {
        let mut parser = crate::parser::Parser::new(&tokens);
        if let Some(Ok(type_spec)) = parser.try_parse_type_spec() {
            if parser.peek() == &crate::lexer::TokenKind::Eof {
                validate_visible_type_spec(ctx, span, &type_spec);
                return;
            }
        }
    }

    validate_visible_named_type(ctx, span, raw.trim());
}

fn validate_supported_character_type_info(ctx: &mut Ctx<'_>, span: Span, info: Option<TypeInfo>) {
    let Some(TypeInfo::Character {
        kind: Some(kind), ..
    }) = info
    else {
        return;
    };
    if kind != 1 {
        ctx.error(
            span,
            format!(
                "CHARACTER(kind={kind}) data is not supported: the backend and runtime support only CHARACTER(kind=1)"
            ),
        );
    }
}

fn character_literal_kind(ctx: &Ctx<'_>, kind: &str) -> Option<i128> {
    kind.parse::<i128>().ok().or_else(|| {
        ctx.lookup_lexical(kind)
            .and_then(|symbol| symbol.const_value)
            .map(i128::from)
    })
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
            let Some(sym) = ctx.lookup_lexical(name) else {
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
        Expr::FunctionCall { .. } => {
            let Some(value) = crate::sema::types::resolve_intrinsic_kind_value(expr, ctx.st) else {
                return Ok(None);
            };
            checked_int_value(
                value,
                crate::driver::defaults::default_int_kind(),
                expr.span,
            )
            .map(Some)
        }
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
        // C pointer interop has additional argument constraints beyond the
        // shared direct-interface validator below.
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

fn smp_intent_name(intent: Option<Intent>) -> &'static str {
    match intent {
        Some(Intent::In) => "INTENT(IN)",
        Some(Intent::Out) => "INTENT(OUT)",
        Some(Intent::InOut) => "INTENT(INOUT)",
        None => "no INTENT",
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

    let proc_lc = name.to_lowercase();
    let Some(interface_owner) = ctx
        .st
        .find_separate_module_interface_scope(parent_mod, &proc_lc)
    else {
        let ancestor_name = ctx
            .st
            .scope(submod_id)
            .submodule_ancestor
            .as_deref()
            .unwrap_or(match &ctx.st.scope(parent_mod).kind {
                ScopeKind::Module(name) | ScopeKind::Submodule(name) => name.as_str(),
                _ => "<unknown>",
            });
        ctx.error(
            span,
            format!(
                "separate module procedure '{name}' has no matching interface in ancestor \
                 module '{ancestor_name}' (F2008 C1414)"
            ),
        );
        return;
    };

    // Locate the signature scope, tolerating one Interface-block hop for a
    // source declaration and a direct child for an .amod-loaded declaration.
    let iface = ctx.st.all_scopes().iter().find_map(|s| {
        let nm = match &s.kind {
            ScopeKind::Function(n) | ScopeKind::Subroutine(n) => n,
            _ => return None,
        };
        if !nm.eq_ignore_ascii_case(&proc_lc) {
            return None;
        }
        let p = s.parent?;
        if p == interface_owner
            || (matches!(ctx.st.scope(p).kind, ScopeKind::Interface)
                && ctx.st.scope(p).parent == Some(interface_owner))
        {
            Some(s.id)
        } else {
            None
        }
    });
    // The marker proves the declaration exists. A current .amod also carries
    // the signature scope, but keep this fallback diagnostic-safe if a source
    // representation lacks one.
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
        if isym.attrs.intent != bsym.attrs.intent {
            ctx.error(
                span,
                format!(
                    "dummy argument '{ba}' of separate module procedure '{name}' has {}, \
                     which does not match {} in its ancestor interface (F2008 C1418)",
                    smp_intent_name(bsym.attrs.intent),
                    smp_intent_name(isym.attrs.intent)
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
                         argument in a procedure reference",
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
                let derived_constructor = ctx
                    .lookup_lexical(name)
                    .filter(|symbol| matches!(symbol.kind, SymbolKind::DerivedType))
                    .map(|symbol| symbol.name.clone());
                if let Some(type_name) = derived_constructor {
                    let interfaces = ctx.lookup_lexical_named_interfaces(name);
                    let resolves_generic = !interfaces.is_empty()
                        && matches!(
                            resolve_generic_procedure_interface(
                                ctx,
                                name,
                                args,
                                DirectProcedureKind::Function,
                                &interfaces,
                            ),
                            ExplicitInterfaceResolution::Resolved(_)
                        );
                    if !resolves_generic {
                        validate_structure_constructor_component_access(
                            ctx, expr.span, &type_name, args,
                        );
                    }
                }
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
                    check_intrinsic_call_types(ctx, expr.span, name, args);
                }
            }
            validate_explicit_interface_call(ctx, callee, args, DirectProcedureKind::Function);
            // F2023 conditional arguments in FUNCTION references select the
            // argument association per arm on the fn-call lowering path
            // (lower_call_arg_maybe_conditional), the same as CALL.
            let saved = ctx.in_call_arg;
            ctx.in_call_arg = true;
            for arg in args {
                validate_const_int_subscript(ctx, &arg.value);
            }
            ctx.in_call_arg = saved;
        }
        Expr::ArrayConstructor { type_spec, values } => {
            if let Some(type_spec) = type_spec {
                validate_visible_raw_type_spec(ctx, expr.span, type_spec);
            } else {
                validate_untyped_array_constructor_elements(ctx, values);
            }
            for value in values {
                validate_const_int_ac_value(ctx, value);
            }
        }
        Expr::ComponentAccess { base, .. } => {
            validate_component_access(ctx, expr);
            validate_supported_character_type_info(
                ctx,
                expr.span,
                validation_expr_type_info(ctx, expr),
            );
            validate_const_int_expr_tree(ctx, base);
        }
        Expr::ComplexLiteral { real, imag } => {
            validate_const_int_expr_tree(ctx, real);
            validate_const_int_expr_tree(ctx, imag);
        }
        Expr::ParenExpr { inner } => validate_const_int_expr_tree(ctx, inner),
        Expr::Name { name } => {
            let info = ctx
                .lookup_lexical(name)
                .and_then(|symbol| symbol.type_info.clone());
            validate_supported_character_type_info(ctx, expr.span, info);
        }
        Expr::StringLiteral {
            kind: Some(kind), ..
        } => {
            if let Some(kind) = character_literal_kind(ctx, kind) {
                if kind != 1 {
                    ctx.error(
                        expr.span,
                        format!(
                            "CHARACTER(kind={kind}) data is not supported: the backend and runtime support only CHARACTER(kind=1)"
                        ),
                    );
                }
            }
        }
        Expr::RealLiteral { .. }
        | Expr::StringLiteral { kind: None, .. }
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
        Decl::DimensionStmt { entities } => {
            for entity in entities {
                for spec in &entity.array_spec {
                    validate_const_int_array_spec(ctx, spec);
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
            body,
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
            if locality
                .iter()
                .any(|spec| matches!(spec, LocalitySpec::DefaultNone))
            {
                validate_do_concurrent_default_none(ctx, controls, locality, body);
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
        Stmt::Stop { code, quiet } | Stmt::ErrorStop { code, quiet } => {
            if let Some(code) = code {
                validate_const_int_expr_tree(ctx, code);
            }
            if let Some(quiet) = quiet {
                validate_const_int_expr_tree(ctx, quiet);
            }
        }
        Stmt::Return { value } => {
            if let Some(value) = value {
                validate_const_int_expr_tree(ctx, value);
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
    let in_module_specification_part =
        matches!(ctx.st.scope(ctx.scope_id).kind, ScopeKind::Module(_));
    for component in components {
        let attrs = match &component.node {
            Decl::AccessDefault {
                access: Attribute::Private,
            } => {
                if !in_module_specification_part {
                    ctx.error(
                        component.span,
                        "component PRIVATE statement is permitted only for a type defined in a module specification part",
                    );
                }
                continue;
            }
            Decl::TypeDecl { attrs, .. } => attrs,
            _ => continue,
        };
        let public_count = attrs
            .iter()
            .filter(|attr| matches!(attr, Attribute::Public))
            .count();
        let private_count = attrs
            .iter()
            .filter(|attr| matches!(attr, Attribute::Private))
            .count();
        let public = public_count != 0;
        let private = private_count != 0;
        if public && private {
            ctx.error(
                component.span,
                "derived-type component cannot be both PUBLIC and PRIVATE",
            );
        } else if public_count + private_count > 1 {
            ctx.error(
                component.span,
                "derived-type component accessibility is specified more than once",
            );
        }
        if (public || private) && !in_module_specification_part {
            ctx.error(
                component.span,
                "component PUBLIC/PRIVATE attribute is permitted only for a type defined in a module specification part",
            );
        }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReferenceRole {
    Value,
    Callable,
    Type,
}

#[derive(Debug)]
struct NameReference {
    name: String,
    span: Span,
    role: ReferenceRole,
}

#[derive(Default)]
struct ProcedureReferenceFacts {
    references: Vec<NameReference>,
    calls: HashSet<String>,
}

fn collect_name_reference(
    name: &str,
    span: Span,
    role: ReferenceRole,
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    let key = name.to_lowercase();
    if !shadowed.contains(&key) {
        facts.references.push(NameReference {
            name: key,
            span,
            role,
        });
    }
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
            collect_name_reference(name, expr.span, ReferenceRole::Value, shadowed, facts);
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
                    facts.calls.insert(key.clone());
                    facts.references.push(NameReference {
                        name: key,
                        span: callee.span,
                        role: ReferenceRole::Callable,
                    });
                }
            } else {
                collect_reference_expr(callee, shadowed, facts);
            }
            for arg in args {
                collect_reference_subscript(&arg.value, shadowed, facts);
            }
        }
        Expr::ArrayConstructor { type_spec, values } => {
            if let Some(type_name) = type_spec {
                collect_name_reference(type_name, expr.span, ReferenceRole::Type, shadowed, facts);
            }
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
        Expr::IntegerLiteral { kind, .. }
        | Expr::RealLiteral { kind, .. }
        | Expr::StringLiteral { kind, .. }
        | Expr::LogicalLiteral { kind, .. } => {
            if let Some(kind) = kind {
                if kind.parse::<u8>().is_err() {
                    collect_name_reference(kind, expr.span, ReferenceRole::Value, shadowed, facts);
                }
            }
        }
        Expr::BozLiteral { .. } | Expr::NilArgument => {}
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
    span: Span,
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
        TypeSpec::Type(name)
        | TypeSpec::Class(name)
        | TypeSpec::TypeOf(name)
        | TypeSpec::ClassOf(name) => {
            collect_name_reference(name, span, ReferenceRole::Type, shadowed, facts);
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
            collect_reference_type_spec(type_spec, decl.span, shadowed, facts);
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
        Decl::DerivedTypeDef {
            extends,
            components,
            type_bound_procs,
            ..
        } => {
            if let Some(parent) = extends {
                collect_name_reference(parent, decl.span, ReferenceRole::Type, shadowed, facts);
            }
            for binding in type_bound_procs {
                if let Some(interface) = &binding.interface {
                    collect_name_reference(
                        interface,
                        decl.span,
                        ReferenceRole::Type,
                        shadowed,
                        facts,
                    );
                }
            }
            for component in components {
                collect_reference_decl(component, shadowed, facts);
            }
        }
        Decl::AttributeStmt {
            attr: Attribute::Dimension(specs),
            ..
        } => {
            for spec in specs {
                collect_reference_array_spec(spec, shadowed, facts);
            }
        }
        Decl::DimensionStmt { entities } => {
            for entity in entities {
                for spec in &entity.array_spec {
                    collect_reference_array_spec(spec, shadowed, facts);
                }
            }
        }
        Decl::ImplicitStmt { specs } => {
            for spec in specs {
                collect_reference_type_spec(&spec.type_spec, decl.span, shadowed, facts);
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
            Decl::DerivedTypeDef { name, .. } => {
                out.insert(name.to_lowercase());
            }
            Decl::EnumDef {
                type_name,
                enumerators,
            } => {
                out.extend(type_name.iter().map(|name| name.to_lowercase()));
                out.extend(enumerators.iter().map(|(name, _)| name.to_lowercase()));
            }
            Decl::EnumerationTypeDef { name, enumerators } => {
                out.insert(name.to_lowercase());
                out.extend(enumerators.iter().map(|name| name.to_lowercase()));
            }
            Decl::AttributeStmt { entities, .. } => {
                out.extend(entities.iter().map(|name| name.to_lowercase()));
            }
            Decl::DimensionStmt { entities } => {
                out.extend(entities.iter().map(|entity| entity.name.to_lowercase()));
            }
            Decl::CommonBlock { vars, .. } => {
                out.extend(vars.iter().map(|name| name.to_lowercase()));
            }
            _ => {}
        }
    }
}

fn collect_block_named_type_names(decls: &[SpannedDecl], out: &mut HashSet<String>) {
    for decl in decls {
        match &decl.node {
            Decl::DerivedTypeDef { name, .. } | Decl::EnumerationTypeDef { name, .. } => {
                out.insert(name.to_lowercase());
            }
            Decl::EnumDef {
                type_name: Some(name),
                ..
            } => {
                out.insert(name.to_lowercase());
            }
            _ => {}
        }
    }
}

fn block_local_bindings(
    decls: &[SpannedDecl],
    ifaces: &[crate::ast::unit::SpannedUnit],
) -> Vec<(String, Span, bool)> {
    use crate::ast::unit::{InterfaceBody, ProgramUnit};

    let mut bindings = Vec::new();
    for decl in decls {
        let mut push = |name: &str| bindings.push((name.to_lowercase(), decl.span, false));
        match &decl.node {
            Decl::TypeDecl { entities, .. } => {
                for entity in entities {
                    push(&entity.name);
                }
            }
            Decl::ParameterStmt { pairs } => {
                for (name, _) in pairs {
                    push(name);
                }
            }
            Decl::DerivedTypeDef { name, .. } => push(name),
            Decl::EnumDef {
                type_name,
                enumerators,
            } => {
                if let Some(name) = type_name {
                    push(name);
                }
                for (name, _) in enumerators {
                    push(name);
                }
            }
            Decl::EnumerationTypeDef { name, enumerators } => {
                push(name);
                for name in enumerators {
                    push(name);
                }
            }
            Decl::AttributeStmt { entities, .. } => {
                for name in entities {
                    push(name);
                }
            }
            Decl::DimensionStmt { entities } => {
                for entity in entities {
                    push(&entity.name);
                }
            }
            Decl::CommonBlock { vars, .. } => {
                for name in vars {
                    push(name);
                }
            }
            _ => {}
        }
    }

    for iface in ifaces {
        let ProgramUnit::InterfaceBlock { name, bodies, .. } = &iface.node else {
            continue;
        };
        if let Some(name) = name.as_ref().filter(|name| !name.is_empty()) {
            bindings.push((name.to_lowercase(), iface.span, true));
        }
        for body in bodies {
            let InterfaceBody::Subprogram(subprogram) = body else {
                continue;
            };
            match &subprogram.node {
                ProgramUnit::Function { name, .. } | ProgramUnit::Subroutine { name, .. } => {
                    bindings.push((name.to_lowercase(), subprogram.span, false));
                }
                _ => {}
            }
        }
    }

    bindings.sort_by(|left, right| {
        (left.1.file_id, left.1.start.line, left.1.start.col, &left.0).cmp(&(
            right.1.file_id,
            right.1.start.line,
            right.1.start.col,
            &right.0,
        ))
    });
    bindings
}

fn validate_block_use_conflicts(
    ctx: &mut Ctx<'_>,
    uses: &[SpannedDecl],
    ifaces: &[crate::ast::unit::SpannedUnit],
    decls: &[SpannedDecl],
) {
    let mut reported = HashSet::new();
    for (name, span, is_generic) in block_local_bindings(decls, ifaces) {
        if !reported.insert(name.clone()) {
            continue;
        }
        let associations = block_use_associations_for_name(ctx.st, uses, &name);
        if ctx
            .st
            .use_associations_conflict_with_local(&associations, is_generic)
        {
            ctx.error(
                span,
                format!(
                    "local declaration '{}' conflicts with a USE-associated entity",
                    name
                ),
            );
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
                collect_name_reference(var, stmt.span, ReferenceRole::Value, shadowed, facts);
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
                                facts.references.push(NameReference {
                                    name: key.clone(),
                                    span: stmt.span,
                                    role: ReferenceRole::Value,
                                });
                            }
                            nested_shadowed.insert(key);
                        }
                    }
                    LocalitySpec::Shared(names) => {
                        for name in names {
                            collect_name_reference(
                                name,
                                stmt.span,
                                ReferenceRole::Value,
                                shadowed,
                                facts,
                            );
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
                    TypeGuard::TypeIs { type_name, body }
                    | TypeGuard::ClassIs { type_name, body } => {
                        collect_name_reference(
                            type_name,
                            stmt.span,
                            ReferenceRole::Type,
                            shadowed,
                            facts,
                        );
                        body
                    }
                    TypeGuard::ClassDefault { body } => body,
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
        // BLOCK references are validated when that lexical scope is entered.
        Stmt::Block { .. } => {}
        Stmt::Associate { assocs, body, .. } => {
            for (_, expr) in assocs {
                collect_reference_expr(expr, shadowed, facts);
            }
            let mut nested_shadowed = shadowed.clone();
            nested_shadowed.extend(assocs.iter().map(|(name, _)| name.to_lowercase()));
            collect_reference_stmts(body, &nested_shadowed, facts);
        }
        Stmt::Stop { code, quiet } | Stmt::ErrorStop { code, quiet } => {
            if let Some(code) = code {
                collect_reference_expr(code, shadowed, facts);
            }
            if let Some(quiet) = quiet {
                collect_reference_expr(quiet, shadowed, facts);
            }
        }
        Stmt::Return { value } => {
            if let Some(value) = value {
                collect_reference_expr(value, shadowed, facts);
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
                collect_reference_type_spec(type_spec, stmt.span, shadowed, facts);
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
                    facts.calls.insert(key.clone());
                    facts.references.push(NameReference {
                        name: key,
                        span: callee.span,
                        role: ReferenceRole::Callable,
                    });
                }
            } else {
                collect_reference_expr(callee, shadowed, facts);
            }
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
                    collect_name_reference(name, stmt.span, ReferenceRole::Value, shadowed, facts);
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

/// `collect_reference_stmt` deliberately leaves BLOCK bodies to the lexical
/// validation pass. DEFAULT(NONE), however, governs the complete
/// do-concurrent-block, including a BLOCK nested under another executable
/// construct. Walk just those skipped lexical islands here while carrying
/// every intervening construct entity that can shadow an outer variable.
fn collect_default_none_nested_block_references(
    st: &SymbolTable,
    stmts: &[SpannedStmt],
    shadowed: &HashSet<String>,
    facts: &mut ProcedureReferenceFacts,
) {
    for stmt in stmts {
        match &stmt.node {
            Stmt::Block {
                uses,
                ifaces,
                decls,
                body,
                ..
            } => {
                let mut nested_shadowed = shadowed.clone();
                collect_block_use_binding_names(st, uses, &mut nested_shadowed);
                collect_block_binding_names(decls, &mut nested_shadowed);
                extend_declared_names_from_ifaces(&mut nested_shadowed, ifaces);
                for decl in decls {
                    collect_reference_decl(decl, &nested_shadowed, facts);
                }
                collect_reference_stmts(body, &nested_shadowed, facts);
                collect_default_none_nested_block_references(st, body, &nested_shadowed, facts);
            }
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_default_none_nested_block_references(st, then_body, shadowed, facts);
                for (_, body) in else_ifs {
                    collect_default_none_nested_block_references(st, body, shadowed, facts);
                }
                if let Some(body) = else_body {
                    collect_default_none_nested_block_references(st, body, shadowed, facts);
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::Labeled { stmt: action, .. } => {
                collect_default_none_nested_block_references(
                    st,
                    std::slice::from_ref(action.as_ref()),
                    shadowed,
                    facts,
                );
            }
            Stmt::DoLoop { body, .. } | Stmt::DoWhile { body, .. } => {
                collect_default_none_nested_block_references(st, body, shadowed, facts);
            }
            Stmt::DoConcurrent {
                controls,
                locality,
                body,
                ..
            } => {
                let mut nested_shadowed = shadowed.clone();
                nested_shadowed.extend(controls.iter().map(|control| control.var.to_lowercase()));
                for spec in locality {
                    match spec {
                        LocalitySpec::Local(names)
                        | LocalitySpec::LocalInit(names)
                        | LocalitySpec::Reduce { vars: names, .. } => {
                            nested_shadowed.extend(names.iter().map(|name| name.to_lowercase()));
                        }
                        LocalitySpec::Shared(_) | LocalitySpec::DefaultNone => {}
                    }
                }
                collect_default_none_nested_block_references(st, body, &nested_shadowed, facts);
            }
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    collect_default_none_nested_block_references(st, &case.body, shadowed, facts);
                }
            }
            Stmt::SelectType {
                assoc_name, guards, ..
            } => {
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
                    collect_default_none_nested_block_references(st, body, &nested_shadowed, facts);
                }
            }
            Stmt::SelectRank {
                assoc_name, guards, ..
            } => {
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
                    collect_default_none_nested_block_references(st, body, &nested_shadowed, facts);
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                collect_default_none_nested_block_references(st, body, shadowed, facts);
                for (_, body) in elsewhere {
                    collect_default_none_nested_block_references(st, body, shadowed, facts);
                }
            }
            Stmt::ForallConstruct { specs, body, .. } => {
                let mut nested_shadowed = shadowed.clone();
                nested_shadowed.extend(specs.iter().map(|spec| spec.var.to_lowercase()));
                collect_default_none_nested_block_references(st, body, &nested_shadowed, facts);
            }
            Stmt::ForallStmt { specs, stmt, .. } => {
                let mut nested_shadowed = shadowed.clone();
                nested_shadowed.extend(specs.iter().map(|spec| spec.var.to_lowercase()));
                collect_default_none_nested_block_references(
                    st,
                    std::slice::from_ref(stmt.as_ref()),
                    &nested_shadowed,
                    facts,
                );
            }
            Stmt::Associate { assocs, body, .. } => {
                let mut nested_shadowed = shadowed.clone();
                nested_shadowed.extend(assocs.iter().map(|(name, _)| name.to_lowercase()));
                collect_default_none_nested_block_references(st, body, &nested_shadowed, facts);
            }
            _ => {}
        }
    }
}

fn validate_do_concurrent_default_none(
    ctx: &mut Ctx<'_>,
    controls: &[ConcurrentControl],
    locality: &[LocalitySpec],
    body: &[SpannedStmt],
) {
    let mut explicitly_localized: HashSet<String> = controls
        .iter()
        .map(|control| control.var.to_lowercase())
        .collect();
    for spec in locality {
        match spec {
            LocalitySpec::Local(names)
            | LocalitySpec::LocalInit(names)
            | LocalitySpec::Shared(names)
            | LocalitySpec::Reduce { vars: names, .. } => {
                explicitly_localized.extend(names.iter().map(|name| name.to_lowercase()));
            }
            LocalitySpec::DefaultNone => {}
        }
    }

    let mut facts = ProcedureReferenceFacts::default();
    collect_reference_stmts(body, &explicitly_localized, &mut facts);
    collect_default_none_nested_block_references(ctx.st, body, &explicitly_localized, &mut facts);

    let mut missing = HashMap::<String, Span>::new();
    for reference in facts.references {
        let Some(symbol) = ctx.lookup_lexical(&reference.name) else {
            continue;
        };
        if !matches!(
            symbol.kind,
            SymbolKind::Variable | SymbolKind::ProcedurePointer
        ) {
            continue;
        }
        missing.entry(reference.name).or_insert(reference.span);
    }

    let mut missing: Vec<_> = missing.into_iter().collect();
    missing.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, span) in missing {
        ctx.error(
            span,
            format!(
                "variable '{name}' referenced in a DO CONCURRENT with DEFAULT(NONE) \
                 must appear in a locality-spec (F2023 C1134)"
            ),
        );
    }
}

fn procedure_reference_facts(unit: &ProgramUnit, unit_span: Span) -> ProcedureReferenceFacts {
    let shadowed = HashSet::new();
    let mut facts = ProcedureReferenceFacts::default();
    let (decls, body) = match unit {
        ProgramUnit::Program { decls, body, .. } | ProgramUnit::Subroutine { decls, body, .. } => {
            (decls.as_slice(), body.as_slice())
        }
        ProgramUnit::Function {
            return_type,
            decls,
            body,
            ..
        } => {
            if let Some(return_type) = return_type {
                collect_reference_type_spec(return_type, unit_span, &shadowed, &mut facts);
            }
            (decls.as_slice(), body.as_slice())
        }
        ProgramUnit::Module { decls, .. }
        | ProgramUnit::Submodule { decls, .. }
        | ProgramUnit::BlockData { decls, .. } => (decls.as_slice(), &[] as &[SpannedStmt]),
        ProgramUnit::InterfaceBlock { .. } => return facts,
    };
    for decl in decls {
        collect_reference_decl(decl, &shadowed, &mut facts);
    }
    collect_reference_stmts(body, &shadowed, &mut facts);
    facts
}

fn procedure_specification_reference_facts(
    unit: &ProgramUnit,
    unit_span: Span,
) -> ProcedureReferenceFacts {
    let shadowed = HashSet::new();
    let mut facts = ProcedureReferenceFacts::default();
    let decls = match unit {
        ProgramUnit::Program { decls, .. } | ProgramUnit::Subroutine { decls, .. } => {
            decls.as_slice()
        }
        ProgramUnit::Function {
            return_type, decls, ..
        } => {
            if let Some(return_type) = return_type {
                collect_reference_type_spec(return_type, unit_span, &shadowed, &mut facts);
            }
            decls.as_slice()
        }
        ProgramUnit::Module { decls, .. }
        | ProgramUnit::Submodule { decls, .. }
        | ProgramUnit::BlockData { decls, .. } => decls.as_slice(),
        ProgramUnit::InterfaceBlock { .. } => return facts,
    };
    for decl in decls {
        collect_reference_decl(decl, &shadowed, &mut facts);
    }
    facts
}

fn program_unit_use_decls(unit: &ProgramUnit) -> &[SpannedDecl] {
    match unit {
        ProgramUnit::Program { uses, .. }
        | ProgramUnit::Module { uses, .. }
        | ProgramUnit::Submodule { uses, .. }
        | ProgramUnit::Subroutine { uses, .. }
        | ProgramUnit::Function { uses, .. }
        | ProgramUnit::BlockData { uses, .. } => uses,
        ProgramUnit::InterfaceBlock { .. } => &[],
    }
}

fn sorted_use_binding_names(st: &SymbolTable, uses: &[SpannedDecl]) -> Vec<String> {
    let mut names = HashSet::new();
    collect_block_use_binding_names(st, uses, &mut names);
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    names
}

fn report_indistinguishable_merged_generic(
    ctx: &mut Ctx<'_>,
    span: Span,
    generic_name: &str,
    interfaces: Option<&[&Symbol]>,
) {
    let pair = interfaces.map_or_else(
        || indistinguishable_merged_generic_specifics(ctx, generic_name),
        |interfaces| indistinguishable_generic_specifics_from_interfaces(ctx, interfaces),
    );
    let Some(pair) = pair else {
        return;
    };
    let report_key = (
        generic_name.to_ascii_lowercase(),
        pair.left_owner_scope,
        pair.left_name.clone(),
        pair.right_owner_scope,
        pair.right_name.clone(),
    );
    if ctx.reported_indistinguishable_generics.insert(report_key) {
        ctx.error(
            span,
            format!(
                "generic interface '{}' has indistinguishable specific procedures \
                 '{}' and '{}'",
                generic_name, pair.left_name, pair.right_name
            ),
        );
    }
}

fn validate_use_ambiguities(ctx: &mut Ctx<'_>, unit: &SpannedUnit) {
    for reference in procedure_specification_reference_facts(&unit.node, unit.span).references {
        if reference.role == ReferenceRole::Callable && is_intrinsic_name(&reference.name) {
            continue;
        }
        if ctx
            .st
            .inaccessible_host_symbol(
                ctx.scope_id,
                &reference.name,
                reference.role == ReferenceRole::Callable,
            )
            .is_none()
        {
            continue;
        }
        let report_key = (ctx.scope_id, reference.name.clone());
        if ctx.reported_inaccessible_host_entities.insert(report_key) {
            ctx.error(
                reference.span,
                format!(
                    "host entity '{}' is not accessible under this IMPORT policy",
                    reference.name
                ),
            );
        }
    }

    // F2023 15.4.3.4.5 constrains every pair of specifics that is accessible
    // under one generic identifier; a call is not required to trigger the
    // constraint. Validate each name introduced by this unit's USE statements
    // once up front, including names re-exported through a single module.
    let mut checked_merged_generics = HashSet::new();
    for name in sorted_use_binding_names(ctx.st, program_unit_use_decls(&unit.node)) {
        checked_merged_generics.insert(name.clone());
        report_indistinguishable_merged_generic(ctx, unit.span, &name, None);
    }

    let facts = procedure_reference_facts(&unit.node, unit.span);
    for reference in facts.references {
        let allow_generic_merge = reference.role == ReferenceRole::Callable;
        if let Some(ambiguity) =
            ctx.st
                .use_ambiguity_in(ctx.scope_id, &reference.name, allow_generic_merge)
        {
            let report_key = (ambiguity.origin_scope, reference.name.clone());
            if !ctx.reported_use_ambiguities.insert(report_key) {
                continue;
            }
            ctx.error(
                reference.span,
                use_ambiguity_message(&reference.name, &ambiguity.providers),
            );
            continue;
        }
        if !allow_generic_merge {
            continue;
        }
        // Local generic declarations can extend a host-associated generic
        // without introducing another USE name in this unit. Retain the
        // reference-time fallback for that case, but never rebuild imported
        // characteristics for every call.
        if checked_merged_generics.insert(reference.name.clone()) {
            report_indistinguishable_merged_generic(ctx, reference.span, &reference.name, None);
        }
    }
}

fn use_ambiguity_message(name: &str, providers: &[String]) -> String {
    let providers = match providers {
        [left, right] => format!("'{}' and '{}'", left, right),
        providers => providers
            .iter()
            .map(|provider| format!("'{}'", provider))
            .collect::<Vec<_>>()
            .join(", "),
    };
    format!(
        "ambiguous USE-associated reference '{}' from modules {}",
        name, providers
    )
}

fn block_use_associations_for_name(
    st: &SymbolTable,
    uses: &[SpannedDecl],
    name: &str,
) -> Vec<UseAssociation> {
    use crate::ast::decl::OnlyItem;

    let key = name.to_lowercase();
    let mut associations = Vec::new();
    for use_decl in uses {
        let Decl::UseStmt {
            module,
            nature,
            renames,
            only,
        } = &use_decl.node
        else {
            continue;
        };
        let Some(source_scope) = find_use_module_scope(st, module, *nature) else {
            continue;
        };
        let original_name = if let Some(items) = only {
            items.iter().find_map(|item| match item {
                OnlyItem::Name(item_name) | OnlyItem::Generic(item_name)
                    if item_name.eq_ignore_ascii_case(&key) =>
                {
                    Some(item_name.to_lowercase())
                }
                OnlyItem::Rename(rename) if rename.local.eq_ignore_ascii_case(&key) => {
                    Some(rename.remote.to_lowercase())
                }
                _ => None,
            })
        } else if let Some(rename) = renames
            .iter()
            .find(|rename| rename.local.eq_ignore_ascii_case(&key))
        {
            Some(rename.remote.to_lowercase())
        } else if renames
            .iter()
            .any(|rename| rename.remote.eq_ignore_ascii_case(&key))
        {
            None
        } else {
            Some(key.clone())
        };
        let Some(original_name) = original_name else {
            continue;
        };
        associations.push(UseAssociation {
            local_name: key.clone(),
            original_name,
            source_scope,
            is_submodule_access: false,
            from_bare_use: only.is_none(),
        });
    }
    associations
}

fn lexical_host_visibility(ctx: &Ctx<'_>, name: &str) -> LexicalHostVisibility {
    let key = name.to_lowercase();
    let mut hidden = false;
    for frame in ctx.ambiguity_lexical_frames.iter().rev() {
        match frame {
            AmbiguityLexicalFrame::Associate { bindings } => {
                if bindings.contains(&key) {
                    return if hidden {
                        LexicalHostVisibility::Hidden
                    } else {
                        LexicalHostVisibility::Visible
                    };
                }
            }
            AmbiguityLexicalFrame::Block {
                uses,
                bindings,
                import_control,
                ..
            } => {
                let associations = block_use_associations_for_name(ctx.st, uses, &key);
                let use_binding = ctx.st.use_associations_bind_name(&associations, &key)
                    || ctx
                        .st
                        .use_ambiguity_from_associations(ctx.scope_id, &key, &associations, true)
                        .is_some();
                if bindings.contains(&key) || use_binding {
                    return if hidden {
                        LexicalHostVisibility::Hidden
                    } else {
                        LexicalHostVisibility::Visible
                    };
                }
                if !import_control.policy.allows(&key) {
                    hidden = true;
                }
            }
        }
    }

    let symbol_exists = ctx.st.lookup_in(ctx.scope_id, &key).is_some()
        || ctx
            .st
            .named_interface_facet_symbol_in_scope(ctx.scope_id, &key)
            .is_some();
    if symbol_exists {
        return if hidden {
            LexicalHostVisibility::Hidden
        } else {
            LexicalHostVisibility::Visible
        };
    }
    if hidden
        && ctx
            .st
            .inaccessible_host_symbol(ctx.scope_id, &key, true)
            .is_some()
    {
        LexicalHostVisibility::Hidden
    } else {
        LexicalHostVisibility::Missing
    }
}

fn collect_block_use_binding_names(
    st: &SymbolTable,
    uses: &[SpannedDecl],
    out: &mut HashSet<String>,
) {
    for use_decl in uses {
        let Decl::UseStmt {
            module,
            nature,
            renames,
            only,
        } = &use_decl.node
        else {
            continue;
        };
        let Some(source_scope) = find_use_module_scope(st, module, *nature) else {
            continue;
        };
        if let Some(items) = only {
            for item in items {
                let (local, remote) = match item {
                    OnlyItem::Name(name) | OnlyItem::Generic(name) => (name, name),
                    OnlyItem::Rename(rename) => (&rename.local, &rename.remote),
                };
                if st.scope_has_exported_entity(source_scope, remote) {
                    out.insert(local.to_lowercase());
                }
            }
            continue;
        }

        out.extend(
            renames
                .iter()
                .filter(|rename| st.scope_has_exported_entity(source_scope, &rename.remote))
                .map(|rename| rename.local.to_lowercase()),
        );
        let renamed_remote: HashSet<String> = renames
            .iter()
            .map(|rename| rename.remote.to_lowercase())
            .collect();
        let source = st.scope(source_scope);
        for symbol in source.symbols.values() {
            let key = symbol.name.to_lowercase();
            if symbol.attrs.access != Access::Private && !renamed_remote.contains(&key) {
                out.insert(key);
            }
        }
        for association in &source.use_associations {
            let key = association.local_name.to_lowercase();
            if !key.is_empty()
                && !renamed_remote.contains(&key)
                && st.scope_exports_name(source_scope, &key)
            {
                out.insert(key);
            }
        }
    }
}

fn validate_block_imports(
    ctx: &mut Ctx<'_>,
    span: Span,
    block_name: Option<&str>,
    uses: &[SpannedDecl],
    imports: &[ImportStmt],
    ifaces: &[SpannedUnit],
    decls: &[SpannedDecl],
) -> BlockImportControl {
    let has_only = imports
        .iter()
        .any(|import| matches!(import, ImportStmt::Only(_)));
    if has_only
        && imports
            .iter()
            .any(|import| !matches!(import, ImportStmt::Only(_)))
    {
        ctx.error(
            span,
            "IMPORT, ONLY cannot be combined with another IMPORT form",
        );
    }
    if imports.len() > 1
        && imports
            .iter()
            .any(|import| matches!(import, ImportStmt::All | ImportStmt::None))
    {
        ctx.error(
            span,
            "IMPORT, ALL and IMPORT, NONE must be the only IMPORT statement in a scope",
        );
    }

    let imported_names: HashSet<String> = imports
        .iter()
        .flat_map(|import| match import {
            ImportStmt::Default(names) | ImportStmt::Only(names) => names.as_slice(),
            ImportStmt::All | ImportStmt::None => &[],
        })
        .map(|name| name.to_lowercase())
        .collect();
    for name in &imported_names {
        if lexical_host_visibility(ctx, name) != LexicalHostVisibility::Visible {
            ctx.error(
                span,
                format!("IMPORT name '{}' does not identify a host entity", name),
            );
        }
    }

    let control = match imports {
        [] => BlockImportControl {
            policy: HostAssociationPolicy::All,
            protection: HostImportProtection::None,
        },
        [ImportStmt::All] => BlockImportControl {
            policy: HostAssociationPolicy::All,
            protection: HostImportProtection::All,
        },
        [ImportStmt::None] => BlockImportControl {
            policy: HostAssociationPolicy::None,
            protection: HostImportProtection::None,
        },
        imports
            if imports
                .iter()
                .all(|import| matches!(import, ImportStmt::Only(_))) =>
        {
            BlockImportControl {
                policy: HostAssociationPolicy::Only(imported_names.clone()),
                protection: HostImportProtection::Names(imported_names),
            }
        }
        _ => BlockImportControl {
            policy: HostAssociationPolicy::All,
            protection: HostImportProtection::Names(imported_names),
        },
    };

    let mut bindings = HashSet::new();
    collect_block_binding_names(decls, &mut bindings);
    extend_declared_names_from_ifaces(&mut bindings, ifaces);
    collect_block_use_binding_names(ctx.st, uses, &mut bindings);
    if let Some(name) = block_name {
        bindings.insert(name.to_lowercase());
    }
    for name in bindings {
        if control.protection.protects(&name)
            && lexical_host_visibility(ctx, &name) == LexicalHostVisibility::Visible
        {
            ctx.error(
                span,
                format!(
                    "BLOCK entity '{}' conflicts with an explicitly imported host entity",
                    name
                ),
            );
        }
    }

    control
}

fn validate_block_use_ambiguities(
    ctx: &mut Ctx<'_>,
    block_span: Span,
    import_control: &BlockImportControl,
    uses: &[SpannedDecl],
    ifaces: &[SpannedUnit],
    implicit: &[SpannedDecl],
    decls: &[SpannedDecl],
    body: &[SpannedStmt],
) {
    let mut shadowed = HashSet::new();
    collect_block_binding_names(decls, &mut shadowed);
    extend_declared_names_from_ifaces(&mut shadowed, ifaces);

    // BLOCK constructs are absent from the ordinary scope graph. Check their
    // explicit USE set here, before reference collection, so an unused merged
    // generic is held to the same constraint as a program-unit generic.
    let mut checked_merged_generics = HashSet::new();
    for name in sorted_use_binding_names(ctx.st, uses) {
        checked_merged_generics.insert(name.clone());
        let associations = block_use_associations_for_name(ctx.st, uses, &name);
        let interfaces = ctx
            .st
            .named_interface_symbols_from_use_associations(&associations, &name);
        report_indistinguishable_merged_generic(ctx, block_span, &name, Some(&interfaces));
    }

    let mut facts = ProcedureReferenceFacts::default();
    for decl in implicit.iter().chain(decls) {
        collect_reference_decl(decl, &shadowed, &mut facts);
    }
    collect_reference_stmts(body, &shadowed, &mut facts);

    for reference in facts.references {
        let allow_generic_merge = reference.role == ReferenceRole::Callable;
        let mut block_ambiguity = None;
        let mut block_binding_found = false;

        let associations = block_use_associations_for_name(ctx.st, uses, &reference.name);
        if allow_generic_merge && checked_merged_generics.insert(reference.name.clone()) {
            let interfaces = ctx
                .st
                .named_interface_symbols_from_use_associations(&associations, &reference.name);
            report_indistinguishable_merged_generic(
                ctx,
                reference.span,
                &reference.name,
                Some(&interfaces),
            );
        }
        if let Some(ambiguity) = ctx.st.use_ambiguity_from_associations(
            ctx.scope_id,
            &reference.name,
            &associations,
            allow_generic_merge,
        ) {
            block_ambiguity = Some((block_span, ambiguity));
        } else if ctx
            .st
            .use_associations_bind_name(&associations, &reference.name)
        {
            block_binding_found = true;
        }

        if block_ambiguity.is_none()
            && !block_binding_found
            && !(allow_generic_merge && is_intrinsic_name(&reference.name))
        {
            let host_visibility = lexical_host_visibility(ctx, &reference.name);
            if host_visibility == LexicalHostVisibility::Hidden
                || (host_visibility == LexicalHostVisibility::Visible
                    && !import_control.policy.allows(&reference.name))
            {
                ctx.error(
                    reference.span,
                    format!(
                        "host entity '{}' is not accessible under this IMPORT policy",
                        reference.name
                    ),
                );
                continue;
            }
        }

        if block_ambiguity.is_none() && !block_binding_found {
            for frame in ctx.ambiguity_lexical_frames.iter().rev() {
                let (origin_span, frame_uses) = match frame {
                    AmbiguityLexicalFrame::Associate { bindings } => {
                        if bindings.contains(&reference.name) {
                            block_binding_found = true;
                            break;
                        }
                        continue;
                    }
                    AmbiguityLexicalFrame::Block {
                        span,
                        uses,
                        bindings,
                        ..
                    } => {
                        if bindings.contains(&reference.name) {
                            block_binding_found = true;
                            break;
                        }
                        (*span, uses.as_slice())
                    }
                };
                let associations =
                    block_use_associations_for_name(ctx.st, frame_uses, &reference.name);
                if let Some(ambiguity) = ctx.st.use_ambiguity_from_associations(
                    ctx.scope_id,
                    &reference.name,
                    &associations,
                    allow_generic_merge,
                ) {
                    block_ambiguity = Some((origin_span, ambiguity));
                    break;
                }
                if ctx
                    .st
                    .use_associations_bind_name(&associations, &reference.name)
                {
                    block_binding_found = true;
                    break;
                }
            }
        }

        if let Some((origin_span, ambiguity)) = block_ambiguity {
            let report_key = (
                origin_span.file_id,
                origin_span.start.line,
                origin_span.start.col,
                reference.name.clone(),
            );
            if ctx.reported_block_use_ambiguities.insert(report_key) {
                ctx.error(
                    reference.span,
                    use_ambiguity_message(&reference.name, &ambiguity.providers),
                );
            }
            continue;
        }
        if block_binding_found {
            continue;
        }

        let Some(ambiguity) =
            ctx.st
                .use_ambiguity_in(ctx.scope_id, &reference.name, allow_generic_merge)
        else {
            continue;
        };
        let report_key = (ambiguity.origin_scope, reference.name.clone());
        if ctx.reported_use_ambiguities.insert(report_key) {
            ctx.error(
                reference.span,
                use_ambiguity_message(&reference.name, &ambiguity.providers),
            );
        }
    }
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
    unit_span: Span,
    caller_scope: ScopeId,
    owner_scope: ScopeId,
    child_names: &HashSet<String>,
) -> HashSet<String> {
    procedure_reference_facts(unit, unit_span)
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

fn validate_finalizer_capture_references(ctx: &mut Ctx<'_>, unit: &SpannedUnit) {
    if ctx.finalizer_capture_host_scopes.is_empty() {
        return;
    }
    let facts = procedure_reference_facts(&unit.node, unit.span);
    for reference in facts.references {
        let Some(symbol) = ctx.st.lookup_in(ctx.scope_id, &reference.name) else {
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
        let key = (ctx.scope_id, host_scope, reference.name.clone());
        if ctx.reported_finalizer_captures.insert(key) {
            ctx.error(
                reference.span,
                format!(
                    "local FINAL procedure cannot reference host entity '{}': deferred finalization cannot preserve procedure host associations; move the state to module storage",
                    reference.name
                ),
            );
        }
    }
}

fn validate_contained_units(
    ctx: &mut Ctx<'_>,
    host: &ProgramUnit,
    host_span: Span,
    contains: &[SpannedUnit],
) {
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
        for callee in resolved_contained_calls(
            ctx,
            host,
            host_span,
            ctx.scope_id,
            ctx.scope_id,
            &child_names,
        ) {
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
            let calls = resolved_contained_calls(
                ctx,
                &unit.node,
                unit.span,
                caller_scope,
                ctx.scope_id,
                &child_names,
            );
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
    validate_use_ambiguities(ctx, unit);
    validate_finalizer_capture_references(ctx, unit);

    match &unit.node {
        ProgramUnit::Program {
            uses,
            implicit,
            decls,
            body,
            contains,
            ..
        } => {
            validate_decls(ctx, uses);
            validate_decls(ctx, implicit);
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
            validate_control_transfer_regions(ctx, body);
            validate_label_consistency(ctx, unit.span);
            validate_contained_units(ctx, &unit.node, unit.span, contains);
        }
        ProgramUnit::Module {
            uses,
            implicit,
            decls,
            contains,
            ..
        } => {
            ctx.require_std(unit.span, FortranStandard::F90, "MODULE");
            validate_decls(ctx, uses);
            validate_decls(ctx, implicit);
            validate_decls(ctx, decls);
            validate_contained_units(ctx, &unit.node, unit.span, contains);
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
            validate_decls(ctx, uses);
            validate_decls(ctx, implicit);
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
            validate_control_transfer_regions(ctx, body);
            validate_label_consistency(ctx, unit.span);
            validate_contained_units(ctx, &unit.node, unit.span, contains);
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
            if let Some(return_type) = return_type {
                validate_supported_character_type_spec(ctx, unit.span, return_type);
                validate_visible_type_spec(ctx, unit.span, return_type);
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
            validate_decls(ctx, uses);
            validate_decls(ctx, implicit);
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
            validate_control_transfer_regions(ctx, body);
            validate_label_consistency(ctx, unit.span);
            validate_contained_units(ctx, &unit.node, unit.span, contains);
            ctx.current_args = saved_args;
            ctx.in_pure = saved_pure;
            ctx.in_elemental = saved_elemental;
            ctx.in_bind_c_unit = saved_bind_c;
        }
        ProgramUnit::Submodule {
            parent,
            ancestor,
            uses,
            implicit,
            decls,
            contains,
            ..
        } => {
            ctx.require_std(unit.span, FortranStandard::F2008, "SUBMODULE");
            // F2008 C1113: a descendant names an exact parent submodule,
            // while a direct child names its ancestor module.
            let parent_exists = if let Some(immediate_parent) = ancestor {
                ctx.st
                    .find_submodule_scope(parent, immediate_parent)
                    .is_some()
            } else {
                ctx.st.find_module_scope(parent).is_some()
            };
            if !parent_exists {
                if let Some(immediate_parent) = ancestor {
                    ctx.error(
                        unit.span,
                        format!(
                            "SUBMODULE immediate parent submodule '{parent}:{immediate_parent}' not found (compile it first and provide its .smod and .amod files)"
                        ),
                    );
                } else {
                    ctx.error(
                        unit.span,
                        format!(
                            "SUBMODULE parent module '{parent}' not found (compile it first or provide its .amod)"
                        ),
                    );
                }
            }
            validate_decls(ctx, uses);
            validate_decls(ctx, implicit);
            validate_decls(ctx, decls);
            validate_contained_units(ctx, &unit.node, unit.span, contains);
        }
        ProgramUnit::BlockData { uses, decls, .. } => {
            warn_legacy_feature(ctx, unit.span, "BLOCK DATA");
            validate_decls(ctx, uses);
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

fn find_use_module_scope(st: &SymbolTable, module: &str, nature: UseNature) -> Option<ScopeId> {
    match nature {
        UseNature::Normal => st.find_module_scope(module),
        UseNature::Intrinsic => st.find_intrinsic_module_scope(module),
        UseNature::NonIntrinsic => st.find_non_intrinsic_module_scope(module),
    }
}

fn validate_use_decl(ctx: &mut Ctx<'_>, decl: &SpannedDecl) {
    let Decl::UseStmt {
        module,
        nature,
        renames,
        only,
    } = &decl.node
    else {
        return;
    };

    let module_scope = find_use_module_scope(ctx.st, module, *nature);
    let Some(module_scope) = module_scope else {
        let msg = match nature {
            UseNature::Normal => format!("module '{}' not found", module),
            UseNature::Intrinsic => format!("module '{}' is not an intrinsic module", module),
            UseNature::NonIntrinsic => {
                format!("non-intrinsic module '{}' not found", module)
            }
        };
        ctx.error(decl.span, msg);
        return;
    };
    if module_scope == ctx.scope_id {
        ctx.error(decl.span, format!("module '{}' cannot USE itself", module));
        return;
    }

    if let Some(items) = only {
        for item in items {
            let target = match item {
                OnlyItem::Name(name) | OnlyItem::Generic(name) => name,
                OnlyItem::Rename(rename) => &rename.remote,
            };
            if !ctx.st.scope_has_exported_entity(module_scope, target) {
                ctx.error(
                    decl.span,
                    format!(
                        "USE target '{}' is not exported by module '{}'",
                        target, module
                    ),
                );
            }
        }
    } else {
        for rename in renames {
            if !ctx
                .st
                .scope_has_exported_entity(module_scope, &rename.remote)
            {
                ctx.error(
                    decl.span,
                    format!(
                        "USE target '{}' is not exported by module '{}'",
                        rename.remote, module
                    ),
                );
            }
        }
    }
}

fn validate_decls(ctx: &mut Ctx, decls: &[crate::ast::decl::SpannedDecl]) {
    for decl in decls {
        validate_decl_const_int_exprs(ctx, decl);
        validate_use_decl(ctx, decl);

        if let Decl::TypeDecl {
            attrs,
            entities,
            type_spec,
            ..
        } = &decl.node
        {
            let has_alloc = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));
            let has_pointer = attrs.iter().any(|a| matches!(a, Attribute::Pointer));
            if has_pointer && type_decl_encodes_procedure_interface(attrs, type_spec) {
                if let TypeSpec::Type(interface_name) = type_spec {
                    let owner_scope = ctx.scope_id;
                    for entity in entities {
                        validate_procedure_pointer_interface(
                            ctx,
                            &entity.name,
                            owner_scope,
                            interface_name,
                            decl.span,
                        );
                        if let Some(initial_target) = entity.ptr_init.as_ref() {
                            validate_procedure_pointer_initializer(
                                ctx,
                                &entity.name,
                                owner_scope,
                                interface_name,
                                initial_target,
                                initial_target.span,
                            );
                        }
                    }
                }
            }

            validate_supported_character_type_spec(ctx, decl.span, type_spec);
            if !type_decl_encodes_procedure_interface(attrs, type_spec) {
                validate_visible_type_spec(ctx, decl.span, type_spec);
            }

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

            // An interoperable assumed-length CHARACTER dummy is passed as a
            // C descriptor, not by armfortas's internal pointer-plus-hidden-
            // length convention. Reject it before lowering or .amod emission
            // until CFI_cdesc_t support is available.
            if ctx.in_bind_c_unit {
                if let TypeSpec::Character(selector) = type_spec {
                    let declared_len = selector.as_ref().and_then(|sel| sel.len.as_ref());
                    for entity in entities {
                        let is_dummy = ctx.current_args.contains(&entity.name.to_lowercase());
                        let effective_len = entity.char_len.as_ref().or(declared_len);
                        if is_dummy
                            && matches!(effective_len, Some(crate::ast::decl::LenSpec::Star))
                        {
                            ctx.error(
                                decl.span,
                                format!(
                                    "BIND(C) assumed-length CHARACTER dummy '{}' is not supported yet \
                                     (C descriptors are not implemented)",
                                    entity.name
                                ),
                            );
                        }
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

        if let Decl::ImplicitStmt { specs } = &decl.node {
            for spec in specs {
                validate_supported_character_type_spec(ctx, decl.span, &spec.type_spec);
            }
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
            extends,
            attrs: type_attrs,
            type_bound_procs,
            components,
            ..
        } = &decl.node
        {
            ctx.require_std(decl.span, FortranStandard::F90, "derived types");
            if let Some(parent) = extends {
                validate_visible_named_type(ctx, decl.span, parent);
            }
            if type_attrs
                .iter()
                .any(|attr| matches!(attr, TypeAttr::Abstract))
            {
                ctx.require_std(decl.span, FortranStandard::F2003, "ABSTRACT type");
            }
            validate_unsupported_component_forms(ctx, components);
            for component in components {
                if let Decl::TypeDecl {
                    type_spec,
                    attrs,
                    entities,
                } = &component.node
                {
                    validate_supported_character_type_spec(ctx, component.span, type_spec);
                    if !type_decl_encodes_procedure_interface(attrs, type_spec) {
                        validate_visible_type_spec(ctx, component.span, type_spec);
                    }
                    let has_pointer = attrs
                        .iter()
                        .any(|attribute| matches!(attribute, Attribute::Pointer));
                    if has_pointer && type_decl_encodes_procedure_interface(attrs, type_spec) {
                        if let TypeSpec::Type(interface_name) = type_spec {
                            let owner_scope = ctx.scope_id;
                            for entity in entities {
                                validate_procedure_pointer_interface(
                                    ctx,
                                    &entity.name,
                                    owner_scope,
                                    interface_name,
                                    component.span,
                                );
                                if let Some(initial_target) = entity.ptr_init.as_ref() {
                                    validate_procedure_pointer_initializer(
                                        ctx,
                                        &entity.name,
                                        owner_scope,
                                        interface_name,
                                        initial_target,
                                        initial_target.span,
                                    );
                                }
                            }
                        }
                    }
                }
            }
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

fn allocation_names_designate_same_entity(ctx: &Ctx<'_>, left: &str, right: &str) -> bool {
    if left.eq_ignore_ascii_case(right) {
        return true;
    }

    match (ctx.lookup_lexical(left), ctx.lookup_lexical(right)) {
        (Some(left), Some(right)) => {
            left.scope == right.scope && left.name.eq_ignore_ascii_case(&right.name)
        }
        _ => false,
    }
}

fn stable_array_reference_callee(ctx: &Ctx<'_>, callee: &SpannedExpr) -> bool {
    if validation_expr_rank(ctx, callee).is_none_or(|rank| rank == 0) {
        return false;
    }

    match &callee.node {
        Expr::Name { name } => {
            if ctx
                .block_binding_attrs(name)
                .and_then(|binding| binding.rank)
                .is_some_and(|rank| rank > 0)
            {
                return true;
            }
            ctx.lookup_lexical(name).is_some_and(|symbol| {
                matches!(symbol.kind, SymbolKind::Variable | SymbolKind::Parameter)
                    && !symbol.attrs.array_spec.is_empty()
            })
        }
        Expr::ComponentAccess { .. } => {
            leaf_field_layout(ctx, callee).is_some_and(|leaf| !leaf.field.procedure_pointer)
        }
        Expr::ParenExpr { inner } => stable_array_reference_callee(ctx, inner),
        _ => false,
    }
}

fn stable_subscript_exprs_are_equal(
    ctx: &Ctx<'_>,
    left: &SpannedExpr,
    right: &SpannedExpr,
) -> bool {
    if let (Ok(Some(left)), Ok(Some(right))) = (
        eval_const_int_expr_checked(ctx, left),
        eval_const_int_expr_checked(ctx, right),
    ) {
        return left.value == right.value;
    }

    allocation_designators_are_equal(ctx, left, right)
}

// This is deliberately proof-oriented rather than ordinary AST equality.
// Procedure calls can return a different subscript on each evaluation, while
// resolved aliases and side-effect-free array references can identify the same
// object even when their source spellings differ.
fn stable_section_subscripts_are_equal(
    ctx: &Ctx<'_>,
    left: &SectionSubscript,
    right: &SectionSubscript,
) -> bool {
    match (left, right) {
        (SectionSubscript::Element(left), SectionSubscript::Element(right)) => {
            stable_subscript_exprs_are_equal(ctx, left, right)
        }
        _ => false,
    }
}

fn allocation_designators_are_equal(
    ctx: &Ctx<'_>,
    left: &SpannedExpr,
    right: &SpannedExpr,
) -> bool {
    match (&left.node, &right.node) {
        (Expr::ParenExpr { inner: left }, _) => allocation_designators_are_equal(ctx, left, right),
        (_, Expr::ParenExpr { inner: right }) => allocation_designators_are_equal(ctx, left, right),
        (Expr::Name { name: left }, Expr::Name { name: right }) => {
            allocation_names_designate_same_entity(ctx, left, right)
        }
        (
            Expr::ComponentAccess {
                base: left_base,
                component: left_component,
            },
            Expr::ComponentAccess {
                base: right_base,
                component: right_component,
            },
        ) => {
            left_component.eq_ignore_ascii_case(right_component)
                && allocation_designators_are_equal(ctx, left_base, right_base)
        }
        (
            Expr::FunctionCall {
                callee: left_callee,
                args: left_args,
            },
            Expr::FunctionCall {
                callee: right_callee,
                args: right_args,
            },
        ) => {
            stable_array_reference_callee(ctx, left_callee)
                && stable_array_reference_callee(ctx, right_callee)
                && allocation_designators_are_equal(ctx, left_callee, right_callee)
                && left_args.len() == right_args.len()
                && left_args.iter().zip(right_args).all(|(left, right)| {
                    left.keyword.is_none()
                        && right.keyword.is_none()
                        && stable_section_subscripts_are_equal(ctx, &left.value, &right.value)
                })
        }
        _ => false,
    }
}

fn allocation_object_designator(item: &SpannedExpr, has_allocation_shape: bool) -> &SpannedExpr {
    if has_allocation_shape {
        if let Expr::FunctionCall { callee, .. } = &item.node {
            return callee;
        }
    }
    item
}

fn validate_distinct_allocation_objects(ctx: &mut Ctx<'_>, items: &[SpannedExpr], statement: &str) {
    let has_allocation_shape = statement == "ALLOCATE";
    for (index, item) in items.iter().enumerate().skip(1) {
        let item_designator = allocation_object_designator(item, has_allocation_shape);
        let earlier = items[..index].iter().find(|earlier| {
            allocation_designators_are_equal(
                ctx,
                allocation_object_designator(earlier, has_allocation_shape),
                item_designator,
            )
        });
        let Some(earlier) = earlier else {
            continue;
        };
        let earlier = earlier.to_sexpr();
        ctx.error(
            item.span,
            format!(
                "{statement} object '{}' designates the same entity as an earlier {statement} object '{earlier}'",
                item.to_sexpr()
            ),
        );
    }
}

fn validate_stop_quiet(ctx: &mut Ctx<'_>, quiet: Option<&SpannedExpr>) {
    let Some(quiet) = quiet else {
        return;
    };
    ctx.require_std(quiet.span, FortranStandard::F2018, "QUIET= specifier");
    let metadata = validation_expr_metadata(ctx, quiet);
    let wrong_type = metadata
        .type_info
        .as_ref()
        .is_some_and(|ty| !matches!(ty, TypeInfo::Logical { .. }));
    let wrong_rank = metadata.rank.is_some_and(|rank| rank != 0);
    if wrong_type || wrong_rank {
        ctx.error(quiet.span, "QUIET= expression must be a scalar LOGICAL");
    }
}

fn validate_arithmetic_if_expr(ctx: &mut Ctx<'_>, expr: &SpannedExpr) {
    let metadata = validation_expr_metadata(ctx, expr);
    let wrong_type = metadata.type_info.as_ref().is_some_and(|ty| {
        !matches!(
            ty,
            TypeInfo::Integer { .. } | TypeInfo::Real { .. } | TypeInfo::DoublePrecision
        )
    });
    let wrong_rank = metadata.rank.is_some_and(|rank| rank != 0);
    if wrong_type || wrong_rank {
        ctx.error(
            expr.span,
            "arithmetic IF expression must be a scalar INTEGER or REAL",
        );
    }
}

fn validate_inquire_stmt(
    ctx: &mut Ctx<'_>,
    stmt_span: Span,
    specs: &[IoControl],
    items: &[SpannedExpr],
) {
    let iolength_specs: Vec<_> = specs
        .iter()
        .filter(|spec| {
            spec.keyword
                .as_deref()
                .is_some_and(|keyword| keyword.eq_ignore_ascii_case("iolength"))
        })
        .collect();

    if iolength_specs.is_empty() {
        if !items.is_empty() {
            ctx.error(stmt_span, "INQUIRE output-item-list requires IOLENGTH=");
        }
        return;
    }

    if specs.len() != 1 || iolength_specs.len() != 1 {
        ctx.error(
            stmt_span,
            "INQUIRE(IOLENGTH=) may not be combined with other specifiers",
        );
    }
    if items.is_empty() {
        ctx.error(stmt_span, "INQUIRE(IOLENGTH=) requires an output-item-list");
    }

    let result = &iolength_specs[0].value;
    let metadata = validation_expr_metadata(ctx, result);
    let scalar_integer = matches!(metadata.type_info, Some(TypeInfo::Integer { .. }))
        && metadata.rank.is_none_or(|rank| rank == 0);
    let definable = actual_is_definable(ctx, result, false).is_none_or(|value| value);
    if !scalar_integer || !definable {
        ctx.error(
            result.span,
            "INQUIRE(IOLENGTH=) result must be a definable scalar INTEGER variable",
        );
    }
}

fn visit_io_branch_labels(stmt: &Stmt, mut visit: impl FnMut(u64)) {
    let controls = match stmt {
        Stmt::Write { controls, .. } | Stmt::Read { controls, .. } => controls,
        Stmt::Open { specs }
        | Stmt::Close { specs }
        | Stmt::Inquire { specs, .. }
        | Stmt::Rewind { specs }
        | Stmt::Backspace { specs }
        | Stmt::Endfile { specs }
        | Stmt::Flush { specs }
        | Stmt::Wait { specs } => specs,
        _ => return,
    };
    for control in controls {
        let is_branch = control.keyword.as_deref().is_some_and(|keyword| {
            matches!(keyword.to_ascii_lowercase().as_str(), "err" | "end" | "eor")
        });
        if !is_branch {
            continue;
        }
        if let Expr::IntegerLiteral { text, .. } = &control.value.node {
            if let Ok(label) = text.parse::<u64>() {
                visit(label);
            }
        }
    }
}

fn validate_stmt(ctx: &mut Ctx, stmt: &SpannedStmt) {
    validate_stmt_const_int_exprs(ctx, stmt);
    validate_stmt_enum_usage(ctx, stmt);
    if ctx.in_pure {
        check_pure_stmt_expr_calls(ctx, stmt);
    }
    visit_io_branch_labels(&stmt.node, |label| {
        ctx.labels_referenced.push((label, stmt.span));
    });

    match &stmt.node {
        // ---- Assignment ----
        Stmt::Assignment { target, value } => {
            validate_assignment_target(ctx, target, stmt.span);
            reject_pure_nonlocal_definition(ctx, target, stmt.span, "assignment");
            let resolves_defined_assignment =
                assignment_resolves_defined_assignment(ctx, target, value);
            validate_intrinsic_assignment(
                ctx,
                target,
                value,
                stmt.span,
                resolves_defined_assignment,
            );
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
            if let Some(type_spec) = type_spec {
                validate_supported_character_type_spec(ctx, stmt.span, type_spec);
                validate_visible_type_spec(ctx, stmt.span, type_spec);
            }
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
            validate_distinct_allocation_objects(ctx, items, "ALLOCATE");
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
            validate_distinct_allocation_objects(ctx, items, "DEALLOCATE");
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
        | Stmt::Rewind { .. }
        | Stmt::Backspace { .. }
        | Stmt::Endfile { .. }
        | Stmt::Flush { .. }
        | Stmt::Wait { .. }
            if ctx.in_pure =>
        {
            ctx.error(stmt.span, "I/O statement not allowed in pure procedure");
        }
        Stmt::Inquire { specs, items } => {
            if ctx.in_pure {
                ctx.error(stmt.span, "I/O statement not allowed in pure procedure");
            }
            validate_inquire_stmt(ctx, stmt.span, specs, items);
        }

        // ---- STOP / ERROR STOP in pure ----
        // F2018 §11.4 forbids STOP in pure procedures; F2023 §11.4 explicitly
        // permits ERROR STOP in pure procedures, which stdlib relies on.
        Stmt::Stop { quiet, .. } => {
            if ctx.in_pure {
                ctx.error(stmt.span, "STOP not allowed in pure procedure");
            }
            validate_stop_quiet(ctx, quiet.as_ref());
        }
        Stmt::ErrorStop { quiet, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F2008, "ERROR STOP");
            validate_stop_quiet(ctx, quiet.as_ref());
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
        Stmt::ArithmeticIf {
            expr,
            neg,
            zero,
            pos,
        } => {
            warn_legacy_feature(ctx, stmt.span, "arithmetic IF");
            validate_arithmetic_if_expr(ctx, expr);
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
        Stmt::SelectType {
            selector,
            assoc_name,
            guards,
            ..
        } => {
            ctx.require_std(stmt.span, FortranStandard::F2003, "SELECT TYPE");
            let refined_name = assoc_name.as_deref().or(match &selector.node {
                Expr::Name { name } => Some(name.as_str()),
                _ => None,
            });
            let selector_type = validation_expr_type_info(ctx, selector);
            let selector_rank = validation_expr_rank(ctx, selector);
            for guard in guards {
                let (body, refined_type) = match guard {
                    TypeGuard::TypeIs { type_name, body }
                    | TypeGuard::ClassIs { type_name, body } => {
                        validate_visible_raw_type_spec(ctx, stmt.span, type_name);
                        (
                            body,
                            validation_array_constructor_type_info(
                                ctx,
                                Some(type_name.as_str()),
                                &[],
                            ),
                        )
                    }
                    TypeGuard::ClassDefault { body } => (body, selector_type.clone()),
                };
                let mut refinement = HashMap::new();
                if let (Some(name), Some(type_info)) = (refined_name, refined_type) {
                    refinement.insert(
                        name.to_lowercase(),
                        BlockBindingAttrs {
                            type_info: Some(type_info),
                            rank: selector_rank,
                            ..BlockBindingAttrs::default()
                        },
                    );
                }
                ctx.block_decl_frames.push(refinement);
                validate_stmts(ctx, body);
                ctx.block_decl_frames.pop();
            }
        }
        Stmt::SelectRank { guards, .. } => {
            ctx.require_std(stmt.span, FortranStandard::F2018, "SELECT RANK");
            for guard in guards {
                let body = match guard {
                    RankGuard::Rank { body, .. }
                    | RankGuard::RankStar { body }
                    | RankGuard::RankDefault { body } => body,
                };
                validate_stmts(ctx, body);
            }
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
            name,
            uses,
            imports,
            ifaces,
            implicit,
            decls,
            body,
        } => {
            ctx.require_std(stmt.span, FortranStandard::F2008, "BLOCK construct");
            let import_control = validate_block_imports(
                ctx,
                stmt.span,
                name.as_deref(),
                uses,
                imports,
                ifaces,
                decls,
            );
            validate_block_use_conflicts(ctx, uses, ifaces, decls);
            validate_block_use_ambiguities(
                ctx,
                stmt.span,
                &import_control,
                uses,
                ifaces,
                implicit,
                decls,
                body,
            );
            validate_decls(ctx, uses);
            for iface in ifaces {
                validate_unit(ctx, iface);
            }
            let mut ambiguity_bindings = HashSet::new();
            collect_block_binding_names(decls, &mut ambiguity_bindings);
            extend_declared_names_from_ifaces(&mut ambiguity_bindings, ifaces);
            let mut named_types = HashSet::new();
            collect_block_named_type_names(decls, &mut named_types);
            ctx.ambiguity_lexical_frames
                .push(AmbiguityLexicalFrame::Block {
                    span: stmt.span,
                    scope_id: ctx.st.statement_block_scope(stmt.span),
                    uses: uses.clone(),
                    bindings: ambiguity_bindings,
                    named_types,
                    import_control: Box::new(import_control),
                });
            validate_decls(ctx, implicit);
            validate_block_dimension_implicit_types(ctx, stmt.span, implicit, decls);
            validate_decls(ctx, decls);
            let frame = block_binding_frame(ctx, stmt.span, implicit, decls);
            ctx.block_decl_frames.push(frame);
            validate_stmts(ctx, body);
            ctx.block_decl_frames.pop();
            ctx.ambiguity_lexical_frames.pop();
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
                    validate_move_alloc_arguments(ctx, args);
                }
                if name.eq_ignore_ascii_case("system_clock") && ctx.lookup(name).is_none() {
                    validate_system_clock_args(ctx, args, stmt.span);
                }
                if name.eq_ignore_ascii_case("random_init")
                    && !intrinsic_name_is_shadowed(ctx, "random_init")
                {
                    ctx.require_std(stmt.span, FortranStandard::F2018, "RANDOM_INIT");
                    validate_random_init_args(ctx, args, stmt.span);
                }
                if name.eq_ignore_ascii_case("execute_command_line")
                    && !intrinsic_name_is_shadowed(ctx, "execute_command_line")
                {
                    validate_execute_command_line_cmdmsg(ctx, args);
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
            validate_explicit_interface_call(ctx, callee, args, DirectProcedureKind::Subroutine);
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
            .associate_binding_type_info(name)
            .and_then(derived_type_name_from_type_info)
            .or_else(|| {
                ctx.block_binding_attrs(name)
                    .and_then(|binding| binding.type_info.as_ref())
                    .and_then(derived_type_name_from_type_info)
            })
            .or_else(|| {
                ctx.lookup_lexical(name)
                    .and_then(|sym| sym.type_info.as_ref())
                    .and_then(derived_type_name_from_type_info)
            }),
        Expr::ParenExpr { inner } => derived_type_name_for_expr(ctx, inner),
        Expr::FunctionCall { callee, .. } => {
            let Expr::Name { name } = &callee.node else {
                return derived_type_name_for_expr(ctx, callee);
            };
            if let Some(type_name) = ctx
                .block_array_element_type_info(name)
                .and_then(derived_type_name_from_type_info)
            {
                return Some(type_name);
            }
            let symbol = ctx.lookup_lexical(name)?;
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

fn symbol_declared_result_rank(symbol: &Symbol) -> Option<usize> {
    if !matches!(
        symbol.kind,
        SymbolKind::Function
            | SymbolKind::ExternalProc
            | SymbolKind::IntrinsicProc
            | SymbolKind::ProcedurePointer
    ) {
        return None;
    }
    let rank = usize::from(symbol.attrs.result_rank).max(symbol.attrs.array_spec.len());
    (rank > 0 || symbol.type_info.is_some()).then_some(rank)
}

fn max_elemental_actual_rank(ctx: &Ctx<'_>, args: &[Argument]) -> Option<usize> {
    args.iter()
        .filter_map(|arg| match &arg.value {
            SectionSubscript::Element(actual) => validation_expr_rank(ctx, actual),
            SectionSubscript::Range { .. } => None,
        })
        .max()
}

fn callable_invocation_rank(ctx: &Ctx<'_>, symbol: &Symbol, args: &[Argument]) -> Option<usize> {
    let declared_rank = symbol_declared_result_rank(symbol)?;
    if symbol.attrs.elemental && declared_rank == 0 {
        max_elemental_actual_rank(ctx, args).or(Some(0))
    } else {
        Some(declared_rank)
    }
}

fn procedure_interface_call_result_metadata(
    ctx: &Ctx<'_>,
    symbol: &Symbol,
    args: &[Argument],
) -> Option<(TypeInfo, usize)> {
    let interface_name = symbol.attrs.procedure_iface.as_deref()?;
    let interface_symbol = ctx
        .st
        .lookup_in(symbol.scope, interface_name)
        .or_else(|| ctx.lookup_lexical(interface_name))?;
    let procedure_scope =
        assignment_candidate_scope(ctx, &interface_symbol.name, interface_symbol.scope)?;
    candidate_result_metadata(ctx, procedure_scope, interface_symbol, args, None)
}

fn component_call_result_metadata(
    ctx: &Ctx<'_>,
    callee: &SpannedExpr,
    args: &[Argument],
) -> Option<(TypeInfo, usize)> {
    let Expr::ComponentAccess { base, component } = &callee.node else {
        return None;
    };

    if let Some(leaf) = leaf_field_layout(ctx, callee) {
        if leaf.field.procedure_pointer {
            let TypeInfo::Derived(interface_name) = &leaf.field.type_info else {
                return None;
            };
            let symbol = ctx
                .lookup_lexical(interface_name)
                .or_else(|| ctx.st.find_symbol_any_scope(interface_name))?;
            return Some((
                symbol.type_info.clone()?,
                callable_invocation_rank(ctx, symbol, args)?,
            ));
        }
        return None;
    }

    let type_name = match validation_expr_type_info(ctx, base)? {
        TypeInfo::Derived(name) | TypeInfo::Class(name) => name,
        _ => return None,
    };
    let layouts = ctx.type_layouts?;
    let layout = layouts
        .get_for_scope(ctx.scope_id, &type_name)
        .or_else(|| layouts.get(&type_name))?;
    let owner_scope = layout.owner_scope.unwrap_or(ctx.scope_id);
    let mut seen = HashSet::new();
    let mut matches = Vec::new();
    for binding in layout.bound_proc_candidates(component) {
        let key = (
            binding.target_name.to_ascii_lowercase(),
            binding.abi_name.to_ascii_lowercase(),
        );
        if !seen.insert(key) {
            continue;
        }
        let Some(scope) = assignment_candidate_scope(ctx, &binding.target_name, owner_scope)
            .or_else(|| assignment_candidate_scope(ctx, &binding.abi_name, owner_scope))
        else {
            continue;
        };
        let Some(symbol) = candidate_symbol(ctx, &binding.target_name, owner_scope, scope)
            .or_else(|| candidate_symbol(ctx, &binding.abi_name, owner_scope, scope))
        else {
            continue;
        };
        let passed_object = (!binding.nopass).then_some(base.as_ref());
        if !call_candidate_matches(ctx, scope, symbol, args, passed_object) {
            continue;
        }
        if let Some(metadata) = candidate_result_metadata(ctx, scope, symbol, args, passed_object) {
            matches.push(metadata);
        }
    }
    let [metadata] = matches.as_slice() else {
        return None;
    };
    Some(metadata.clone())
}

fn validation_array_constructor_type_info(
    ctx: &Ctx<'_>,
    type_spec: Option<&str>,
    values: &[AcValue],
) -> Option<TypeInfo> {
    if let Some(raw) = type_spec {
        let tokens = crate::lexer::tokenize(raw, 0, crate::lexer::SourceForm::FreeForm).ok()?;
        let mut parser = crate::parser::Parser::new(&tokens);
        if let Some(Ok(parsed)) = parser.try_parse_type_spec() {
            if parser.peek() == &crate::lexer::TokenKind::Eof {
                return Some(
                    crate::sema::resolve::type_resolution::type_spec_to_info_in_scope(
                        &parsed,
                        ctx.st,
                        ctx.lexical_scope_id(),
                    ),
                );
            }
        }

        let symbol = ctx.lookup_lexical(raw.trim())?;
        if matches!(symbol.kind, SymbolKind::DerivedType) {
            return Some(TypeInfo::Derived(symbol.name.clone()));
        }
    }

    values.iter().find_map(|value| match value {
        AcValue::Expr(expr) => validation_expr_type_info(ctx, expr),
        AcValue::ImpliedDo(implied) => {
            validation_array_constructor_type_info(ctx, None, &implied.values)
        }
    })
}

fn validate_untyped_array_constructor_elements(ctx: &mut Ctx<'_>, values: &[AcValue]) {
    let Some(expected) = validation_array_constructor_type_info(ctx, None, values) else {
        return;
    };

    fn validate_values(ctx: &mut Ctx<'_>, expected: &TypeInfo, values: &[AcValue]) {
        for value in values {
            match value {
                AcValue::Expr(expr) => {
                    let Some(actual) = validation_expr_type_info(ctx, expr) else {
                        continue;
                    };
                    if !defined_assignment_type_matches(
                        ctx,
                        ctx.lexical_scope_id(),
                        expected,
                        &actual,
                    ) {
                        ctx.error(
                            expr.span,
                            format!(
                                "array constructor element type mismatch: expected {}, got {}",
                                intrinsic_assignment_type_name(expected),
                                intrinsic_assignment_type_name(&actual)
                            ),
                        );
                    }
                }
                AcValue::ImpliedDo(implied) => {
                    validate_values(ctx, expected, &implied.values);
                }
            }
        }
    }

    validate_values(ctx, &expected, values);
}

#[derive(Clone)]
struct ValidationExprMetadata {
    type_info: Option<TypeInfo>,
    rank: Option<usize>,
}

fn validation_expr_metadata(ctx: &Ctx<'_>, expr: &SpannedExpr) -> ValidationExprMetadata {
    match &expr.node {
        Expr::UnaryOp { op, operand } => {
            let operand_expr = operand.as_ref();
            let operand_metadata = validation_expr_metadata(ctx, operand_expr);
            let interface_name = format!("operator({op})");
            if let Some((type_info, rank)) = defined_operator_result_metadata(
                ctx,
                &interface_name,
                &[operand_expr],
                std::slice::from_ref(&operand_metadata),
            ) {
                return ValidationExprMetadata {
                    type_info: Some(type_info),
                    rank: Some(rank),
                };
            }
            let type_info = operand_metadata.type_info.as_ref().and_then(|type_info| {
                unary_op_result_type(op, &type_info_to_fortran_type(type_info))
                    .and_then(fortran_type_to_validation_type_info)
            });
            ValidationExprMetadata {
                type_info,
                rank: operand_metadata.rank,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let left_metadata = validation_expr_metadata(ctx, left);
            let right_metadata = validation_expr_metadata(ctx, right);
            let interface_name = format!("operator({op})");
            if let Some((type_info, rank)) = defined_operator_result_metadata(
                ctx,
                &interface_name,
                &[left.as_ref(), right.as_ref()],
                &[left_metadata.clone(), right_metadata.clone()],
            ) {
                return ValidationExprMetadata {
                    type_info: Some(type_info),
                    rank: Some(rank),
                };
            }
            let type_info = match (
                left_metadata.type_info.as_ref(),
                right_metadata.type_info.as_ref(),
            ) {
                (Some(left), Some(right)) => binary_op_result_type(
                    op,
                    &type_info_to_fortran_type(left),
                    &type_info_to_fortran_type(right),
                )
                .and_then(fortran_type_to_validation_type_info),
                _ => None,
            };
            let rank = match (left_metadata.rank, right_metadata.rank) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (Some(rank), None) | (None, Some(rank)) => Some(rank),
                (None, None) => None,
            };
            ValidationExprMetadata { type_info, rank }
        }
        _ => ValidationExprMetadata {
            type_info: validation_expr_type_info(ctx, expr),
            rank: validation_expr_rank(ctx, expr),
        },
    }
}

fn validation_expr_type_info(ctx: &Ctx<'_>, expr: &SpannedExpr) -> Option<TypeInfo> {
    if matches!(expr.node, Expr::ComponentAccess { .. }) {
        if let Some(leaf) = leaf_field_layout(ctx, expr) {
            return Some(leaf.field.type_info.clone());
        }
    }
    let resolved = match &expr.node {
        Expr::Name { name } => ctx
            .associate_binding_type_info(name)
            .cloned()
            .or_else(|| {
                ctx.block_binding_attrs(name)
                    .and_then(|binding| binding.type_info.clone())
            })
            .or_else(|| {
                ctx.lookup_lexical(name)
                    .and_then(|symbol| symbol.type_info.clone())
            }),
        Expr::ParenExpr { inner } => validation_expr_type_info(ctx, inner),
        Expr::ComponentAccess { base, component }
            if component.eq_ignore_ascii_case("re") || component.eq_ignore_ascii_case("im") =>
        {
            match validation_expr_type_info(ctx, base) {
                Some(TypeInfo::Complex { kind }) => Some(TypeInfo::Real { kind }),
                _ => None,
            }
        }
        Expr::ArrayConstructor { type_spec, values } => {
            validation_array_constructor_type_info(ctx, type_spec.as_deref(), values)
        }
        Expr::ComplexLiteral { real, imag } => {
            let component_kind =
                |component: &SpannedExpr| match validation_expr_type_info(ctx, component)? {
                    TypeInfo::Real { kind } => Some(default_real_kind(kind)),
                    TypeInfo::DoublePrecision => Some(8),
                    TypeInfo::Integer { .. } => None,
                    _ => None,
                };
            let kind = component_kind(real)
                .into_iter()
                .chain(component_kind(imag))
                .max()
                .unwrap_or_else(crate::driver::defaults::default_real_kind);
            Some(TypeInfo::Complex { kind: Some(kind) })
        }
        Expr::UnaryOp { .. } | Expr::BinaryOp { .. } => {
            validation_expr_metadata(ctx, expr).type_info
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                if let Some((type_info, _)) = component_call_result_metadata(ctx, callee, args) {
                    return Some(type_info);
                }
                let leaf = leaf_field_layout(ctx, expr)?;
                return (!leaf.field.procedure_pointer).then(|| leaf.field.type_info.clone());
            };
            if let Some(type_info) = ctx.block_array_element_type_info(name) {
                return Some(type_info.clone());
            }
            if let Some((type_info, _)) = named_generic_call_result_metadata(ctx, name, args) {
                return Some(type_info);
            }
            let symbol = ctx.lookup_lexical(name);
            if let Some(metadata) = symbol
                .and_then(|symbol| procedure_interface_call_result_metadata(ctx, symbol, args))
            {
                return Some(metadata.0);
            }
            let user_callable = symbol.is_some_and(|symbol| {
                matches!(
                    symbol.kind,
                    SymbolKind::Function
                        | SymbolKind::Subroutine
                        | SymbolKind::ExternalProc
                        | SymbolKind::ProcedurePointer
                        | SymbolKind::NamedInterface
                ) && !symbol.attrs.intrinsic
            });
            if is_intrinsic_name(name) && !user_callable {
                let arg_types: Option<Vec<_>> = args
                    .iter()
                    .map(|arg| match &arg.value {
                        SectionSubscript::Element(expr) => validation_expr_type_info(ctx, expr)
                            .map(|info| type_info_to_fortran_type(&info)),
                        SectionSubscript::Range { .. } => None,
                    })
                    .collect();
                if let Some(arg_types) = arg_types {
                    let key = name.to_ascii_lowercase();
                    let kind_position = crate::sema::types::character_integer_result_kind_position(
                        &key,
                    )
                    .or(match key.as_str() {
                        "cmplx" => Some(2),
                        "size" => Some(2),
                        "int" | "nint" | "floor" | "ceiling" | "real" | "logical" | "char"
                        | "achar" => Some(1),
                        _ => None,
                    });
                    let requested_kind = kind_position
                        .and_then(|position| call_rank_argument_expr(args, position, &["kind"]))
                        .and_then(|kind_expr| {
                            eval_const_int_expr_checked(ctx, kind_expr).ok().flatten()
                        })
                        .and_then(|value| u8::try_from(value.value).ok());
                    if let Some(kind) = requested_kind {
                        let type_info = match key.as_str() {
                            "int" | "nint" | "floor" | "ceiling" | "len" | "len_trim" | "index"
                            | "scan" | "verify" | "ichar" | "iachar" | "size" => {
                                TypeInfo::Integer { kind: Some(kind) }
                            }
                            "real" => TypeInfo::Real { kind: Some(kind) },
                            "cmplx" => TypeInfo::Complex { kind: Some(kind) },
                            "logical" => TypeInfo::Logical { kind: Some(kind) },
                            "char" | "achar" => TypeInfo::Character {
                                len: Some(1),
                                kind: Some(kind),
                            },
                            _ => unreachable!(),
                        };
                        return Some(type_info);
                    }
                    if let Some(type_) = intrinsic_result_type(name, &arg_types) {
                        return fortran_type_to_validation_type_info(type_);
                    }
                }
            }
            symbol.and_then(|symbol| {
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

fn intrinsic_assignment_type_name(type_info: &TypeInfo) -> String {
    match type_info {
        TypeInfo::Integer { kind } => format!("INTEGER({})", default_int_kind(*kind)),
        TypeInfo::Real { kind } => format!("REAL({})", default_real_kind(*kind)),
        TypeInfo::DoublePrecision => "DOUBLE PRECISION".to_string(),
        TypeInfo::Complex { kind } => format!("COMPLEX({})", default_real_kind(*kind)),
        TypeInfo::Logical { kind } => format!("LOGICAL({})", default_int_kind(*kind)),
        TypeInfo::Character { kind, .. } => format!("CHARACTER({})", kind.unwrap_or(1)),
        TypeInfo::Derived(name) => format!("TYPE({name})"),
        TypeInfo::Class(name) => format!("CLASS({name})"),
        TypeInfo::ClassStar => "CLASS(*)".to_string(),
        TypeInfo::TypeStar => "TYPE(*)".to_string(),
        TypeInfo::Enumeration(name) => format!("ENUMERATION({name})"),
    }
}

fn intrinsic_assignment_types_compatible(
    ctx: &Ctx<'_>,
    target: &TypeInfo,
    value: &TypeInfo,
) -> bool {
    let numeric = |type_info: &TypeInfo| {
        matches!(
            type_info,
            TypeInfo::Integer { .. }
                | TypeInfo::Real { .. }
                | TypeInfo::DoublePrecision
                | TypeInfo::Complex { .. }
        )
    };

    if numeric(target) && numeric(value) {
        return true;
    }

    match (target, value) {
        (TypeInfo::Logical { .. }, TypeInfo::Logical { .. }) => true,
        (TypeInfo::Character { kind: target, .. }, TypeInfo::Character { kind: value, .. }) => {
            target.unwrap_or(1) == value.unwrap_or(1)
        }
        (TypeInfo::Derived(target), TypeInfo::Derived(value) | TypeInfo::Class(value)) => {
            assignment_type_names_match(ctx, ctx.scope_id, target, value)
        }
        (TypeInfo::Class(target), TypeInfo::Derived(value) | TypeInfo::Class(value)) => {
            assignment_type_is_same_or_extension(ctx, ctx.scope_id, target, value)
        }
        (TypeInfo::ClassStar, _) => true,
        (TypeInfo::TypeStar, _) | (_, TypeInfo::TypeStar) => true,
        // Enumeration assignment has stricter, name-based diagnostics in
        // validate_stmt_enum_usage.
        (TypeInfo::Enumeration(_), _) | (_, TypeInfo::Enumeration(_)) => true,
        _ => false,
    }
}

fn validation_const_int_value(ctx: &Ctx<'_>, expr: &SpannedExpr) -> Option<i128> {
    eval_const_int_expr_checked(ctx, expr)
        .ok()
        .flatten()
        .map(|value| value.value)
}

fn validation_explicit_dim_bounds(
    ctx: &Ctx<'_>,
    spec: &crate::ast::decl::ArraySpec,
) -> Option<(i128, i128)> {
    let crate::ast::decl::ArraySpec::Explicit { lower, upper } = spec else {
        return None;
    };
    let lower = lower
        .as_ref()
        .map(|lower| validation_const_int_value(ctx, lower))
        .unwrap_or(Some(1))?;
    Some((lower, validation_const_int_value(ctx, upper)?))
}

fn validation_extent(lower: i128, upper: i128) -> Option<i128> {
    if upper < lower {
        Some(0)
    } else {
        upper.checked_sub(lower)?.checked_add(1)
    }
}

fn validation_section_extent(start: i128, end: i128, stride: i128) -> Option<i128> {
    if stride > 0 {
        if end < start {
            Some(0)
        } else {
            end.checked_sub(start)?.checked_div(stride)?.checked_add(1)
        }
    } else if stride < 0 {
        if end > start {
            Some(0)
        } else {
            start
                .checked_sub(end)?
                .checked_div(stride.checked_neg()?)?
                .checked_add(1)
        }
    } else {
        None
    }
}

fn validation_provable_array_shape(ctx: &Ctx<'_>, expr: &SpannedExpr) -> Option<Vec<i128>> {
    match &expr.node {
        Expr::ParenExpr { inner } => validation_provable_array_shape(ctx, inner),
        Expr::Name { name } => {
            let symbol = ctx.lookup_lexical(name)?;
            if symbol.attrs.array_spec.is_empty() {
                return None;
            }
            symbol
                .attrs
                .array_spec
                .iter()
                .map(|spec| {
                    let (lower, upper) = validation_explicit_dim_bounds(ctx, spec)?;
                    validation_extent(lower, upper)
                })
                .collect()
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let symbol = ctx.lookup_lexical(name)?;
            if !matches!(symbol.kind, SymbolKind::Variable | SymbolKind::Parameter)
                || symbol.attrs.array_spec.len() != args.len()
            {
                return None;
            }

            let mut shape = Vec::new();
            for (arg, spec) in args.iter().zip(&symbol.attrs.array_spec) {
                match &arg.value {
                    SectionSubscript::Element(index) => {
                        if validation_expr_rank(ctx, index).is_some_and(|rank| rank > 0) {
                            return None;
                        }
                    }
                    SectionSubscript::Range { start, end, stride } => {
                        let stride = stride
                            .as_ref()
                            .map(|stride| validation_const_int_value(ctx, stride))
                            .unwrap_or(Some(1))?;
                        let declared_bounds = validation_explicit_dim_bounds(ctx, spec);
                        let start = start
                            .as_ref()
                            .map(|start| validation_const_int_value(ctx, start))
                            .unwrap_or_else(|| {
                                declared_bounds.map(
                                    |(lower, upper)| {
                                        if stride > 0 {
                                            lower
                                        } else {
                                            upper
                                        }
                                    },
                                )
                            })?;
                        let end = end
                            .as_ref()
                            .map(|end| validation_const_int_value(ctx, end))
                            .unwrap_or_else(|| {
                                declared_bounds.map(
                                    |(lower, upper)| {
                                        if stride > 0 {
                                            upper
                                        } else {
                                            lower
                                        }
                                    },
                                )
                            })?;
                        shape.push(validation_section_extent(start, end, stride)?);
                    }
                }
            }
            Some(shape)
        }
        _ => None,
    }
}

fn intrinsic_assignment_target_reallocates(ctx: &Ctx<'_>, target: &SpannedExpr) -> bool {
    match &target.node {
        Expr::Name { name } => ctx
            .lookup_lexical(name)
            .is_some_and(|symbol| symbol.attrs.allocatable),
        Expr::ComponentAccess { .. } => {
            leaf_field_layout(ctx, target).is_some_and(|leaf| leaf.field.allocatable)
        }
        _ => false,
    }
}

fn validate_intrinsic_assignment(
    ctx: &mut Ctx<'_>,
    target: &SpannedExpr,
    value: &SpannedExpr,
    span: Span,
    uses_defined_assignment: bool,
) {
    if uses_defined_assignment {
        return;
    }

    let target_rank = validation_expr_rank(ctx, target);
    let value_rank = validation_expr_rank(ctx, value);
    if let (Some(target_rank), Some(value_rank)) = (target_rank, value_rank) {
        if value_rank != 0 && target_rank != value_rank {
            ctx.error(
                span,
                format!(
                    "intrinsic assignment rank mismatch: rank-{value_rank} expression cannot be assigned to rank-{target_rank} variable"
                ),
            );
        }
    }

    let Some(target_type) = validation_expr_type_info(ctx, target) else {
        return;
    };
    let Some(value_type) = validation_expr_type_info(ctx, value) else {
        return;
    };
    let types_compatible = intrinsic_assignment_types_compatible(ctx, &target_type, &value_type);
    if !types_compatible {
        ctx.error(
            value.span,
            format!(
                "intrinsic assignment cannot convert {} to {}",
                intrinsic_assignment_type_name(&value_type),
                intrinsic_assignment_type_name(&target_type)
            ),
        );
    }

    let target_is_derived = matches!(target_type, TypeInfo::Derived(_) | TypeInfo::Class(_));
    if types_compatible
        && target_is_derived
        && value_rank.is_some_and(|rank| rank > 0)
        && !intrinsic_assignment_target_reallocates(ctx, target)
    {
        if let (Some(target_shape), Some(value_shape)) = (
            validation_provable_array_shape(ctx, target),
            validation_provable_array_shape(ctx, value),
        ) {
            if let Some((dimension, (target_extent, value_extent))) = target_shape
                .iter()
                .zip(&value_shape)
                .enumerate()
                .find(|(_, (target_extent, value_extent))| target_extent != value_extent)
            {
                ctx.error(
                    value.span,
                    format!(
                        "intrinsic assignment shape mismatch in dimension {}: target extent {}, value extent {}",
                        dimension + 1,
                        target_extent,
                        value_extent
                    ),
                );
            }
        }
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
        (TypeInfo::Derived(declared), TypeInfo::Derived(actual) | TypeInfo::Class(actual)) => {
            assignment_type_names_match(ctx, declared_scope, declared, actual)
        }
        (TypeInfo::Class(declared), TypeInfo::Class(actual) | TypeInfo::Derived(actual)) => {
            assignment_type_is_same_or_extension(ctx, declared_scope, declared, actual)
        }
        (TypeInfo::ClassStar, _) => true,
        (TypeInfo::TypeStar, TypeInfo::TypeStar)
        | (TypeInfo::DoublePrecision, TypeInfo::DoublePrecision) => true,
        (TypeInfo::Character { kind: declared, .. }, TypeInfo::Character { kind: actual, .. }) => {
            kind_eq(*declared, *actual, 1)
        }
        (TypeInfo::Integer { kind: a }, TypeInfo::Integer { kind: b }) => {
            kind_eq(*a, *b, crate::driver::defaults::default_int_kind())
        }
        (TypeInfo::Real { kind: a }, TypeInfo::Real { kind: b }) => {
            kind_eq(*a, *b, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::Real { kind }, TypeInfo::DoublePrecision)
        | (TypeInfo::DoublePrecision, TypeInfo::Real { kind }) => {
            kind_eq(*kind, Some(8), crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::Complex { kind: a }, TypeInfo::Complex { kind: b }) => {
            kind_eq(*a, *b, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::Logical { kind: a }, TypeInfo::Logical { kind: b }) => {
            kind_eq(*a, *b, crate::driver::defaults::default_int_kind())
        }
        (TypeInfo::Enumeration(a), TypeInfo::Enumeration(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

fn assignment_candidate_scope<'a>(
    ctx: &'a Ctx<'_>,
    name: &str,
    owner_scope: ScopeId,
) -> Option<&'a Scope> {
    let matches_name = |scope: &&Scope| {
        matches!(
            &scope.kind,
            ScopeKind::Function(candidate) | ScopeKind::Subroutine(candidate)
                if candidate.eq_ignore_ascii_case(name)
        )
    };
    ctx.st
        .all_scopes()
        .iter()
        .filter(matches_name)
        .find(|scope| scope.parent == Some(owner_scope))
        .or_else(|| {
            ctx.st
                .all_scopes()
                .iter()
                .filter(matches_name)
                .find(|scope| {
                    scope.parent.is_some_and(|parent| {
                        matches!(ctx.st.scope(parent).kind, ScopeKind::Interface)
                            && ctx.st.scope(parent).parent == Some(owner_scope)
                    })
                })
        })
}

fn interface_specific_owner_scope(ctx: &Ctx<'_>, interface: &Symbol, name: &str) -> ScopeId {
    ctx.st
        .lookup_in(interface.scope, name)
        .filter(|symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Function
                    | SymbolKind::Subroutine
                    | SymbolKind::ExternalProc
                    | SymbolKind::IntrinsicProc
                    | SymbolKind::ProcedurePointer
            )
        })
        .map(|symbol| symbol.scope)
        .unwrap_or(interface.scope)
}

fn defined_interface_candidates(
    ctx: &Ctx<'_>,
    interface_name: &str,
    operand_types: &[&TypeInfo],
) -> Vec<(String, ScopeId)> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |name: &str, owner_scope: ScopeId| {
        let key = (name.to_ascii_lowercase(), owner_scope);
        if seen.insert(key.clone()) {
            candidates.push((key.0, owner_scope));
        }
    };

    for interface in ctx.lookup_lexical_named_interfaces(interface_name) {
        for name in &interface.arg_names {
            push(name, interface_specific_owner_scope(ctx, interface, name));
        }
    }

    if let Some(layouts) = ctx.type_layouts {
        for operand_type in operand_types {
            let type_name = match operand_type {
                TypeInfo::Derived(name) | TypeInfo::Class(name) => name,
                _ => continue,
            };
            let Some(layout) = layouts
                .get_for_scope(ctx.scope_id, type_name)
                .or_else(|| layouts.get(type_name))
            else {
                continue;
            };
            let owner_scope = layout.owner_scope.unwrap_or(ctx.scope_id);
            for binding in layout.bound_proc_candidates(interface_name) {
                push(&binding.target_name, owner_scope);
                if !binding.abi_name.eq_ignore_ascii_case(&binding.target_name) {
                    push(&binding.abi_name, owner_scope);
                }
            }
        }
    }

    candidates
}

fn candidate_symbol<'a>(
    ctx: &'a Ctx<'_>,
    name: &str,
    owner_scope: ScopeId,
    procedure_scope: &Scope,
) -> Option<&'a Symbol> {
    let key = name.to_ascii_lowercase();
    ctx.st.scope(owner_scope).symbols.get(&key).or_else(|| {
        let mut scope_id = procedure_scope.parent;
        while let Some(current) = scope_id {
            if let Some(symbol) = ctx.st.scope(current).symbols.get(&key) {
                return Some(symbol);
            }
            if current == owner_scope {
                break;
            }
            scope_id = ctx.st.scope(current).parent;
        }
        None
    })
}

fn result_symbol_in_procedure_scope(scope: &Scope) -> Option<&Symbol> {
    scope.procedure_result_symbol()
}

fn generic_dummy_type_matches(
    ctx: &Ctx<'_>,
    scope: ScopeId,
    dummy: &TypeInfo,
    actual: &TypeInfo,
) -> bool {
    matches!(dummy, TypeInfo::ClassStar | TypeInfo::TypeStar)
        || defined_assignment_type_matches(ctx, scope, dummy, actual)
}

fn explicit_actual_rank_matches(
    ctx: &Ctx<'_>,
    actual: &SpannedExpr,
    actual_type: Option<&TypeInfo>,
    actual_rank: usize,
    dummy_rank: usize,
    assumed_rank: bool,
    allows_sequence_association: bool,
    dummy_type: Option<&TypeInfo>,
    elemental: bool,
) -> bool {
    if (elemental && dummy_rank == 0) || assumed_rank || actual_rank == dummy_rank {
        return true;
    }
    if !allows_sequence_association {
        return false;
    }
    let character_storage = matches!(
        (dummy_type, actual_type),
        (
            Some(TypeInfo::Character { .. }),
            Some(TypeInfo::Character { .. })
        )
    );
    actual_rank > 0 || is_array_element_designator(ctx, actual) || character_storage
}

fn call_candidate_matches(
    ctx: &Ctx<'_>,
    scope: &Scope,
    symbol: &Symbol,
    args: &[Argument],
    passed_object: Option<&SpannedExpr>,
) -> bool {
    let mut actuals: Vec<(Option<&str>, Option<&SpannedExpr>)> = Vec::with_capacity(args.len() + 1);
    if let Some(object) = passed_object {
        actuals.push((None, Some(object)));
    }
    actuals.extend(args.iter().map(|arg| {
        let expr = match &arg.value {
            SectionSubscript::Element(expr) => Some(expr),
            SectionSubscript::Range { .. } => None,
        };
        (arg.keyword.as_deref(), expr)
    }));

    let mut matched = vec![false; scope.arg_order.len()];
    let mut position = 0;
    let mut seen_keyword = false;
    let mut elemental_rank = None;
    for (keyword, actual) in actuals {
        let dummy_index = if let Some(keyword) = keyword {
            seen_keyword = true;
            let Some(index) = scope
                .arg_order
                .iter()
                .position(|name| name.eq_ignore_ascii_case(keyword))
            else {
                return false;
            };
            index
        } else {
            if seen_keyword {
                return false;
            }
            while position < matched.len() && matched[position] {
                position += 1;
            }
            if position == matched.len() {
                return false;
            }
            let index = position;
            position += 1;
            index
        };
        if matched[dummy_index] {
            return false;
        }
        matched[dummy_index] = true;

        let Some(actual) = actual else {
            continue;
        };
        let Some(dummy) = scope
            .arg_order
            .get(dummy_index)
            .and_then(|name| scope.symbols.get(name))
        else {
            return false;
        };
        let actual_type = validation_expr_type_info(ctx, actual);
        if let (Some(dummy_type), Some(actual_type)) =
            (dummy.type_info.as_ref(), actual_type.as_ref())
        {
            if !generic_dummy_type_matches(ctx, scope.id, dummy_type, actual_type) {
                return false;
            }
        }
        if let Some(actual_rank) = validation_expr_rank(ctx, actual) {
            let dummy_rank = dummy.attrs.array_spec.len();
            if symbol.attrs.elemental && dummy_rank == 0 && actual_rank > 0 {
                if elemental_rank.is_some_and(|rank| rank != actual_rank) {
                    return false;
                }
                elemental_rank = Some(actual_rank);
            }
            let assumed_rank = dummy
                .attrs
                .array_spec
                .iter()
                .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedRank));
            let allows_sequence_association = !dummy.attrs.array_spec.is_empty()
                && dummy.attrs.array_spec.iter().all(|spec| {
                    matches!(
                        spec,
                        crate::ast::decl::ArraySpec::Explicit { .. }
                            | crate::ast::decl::ArraySpec::AssumedSize { .. }
                    )
                });
            if !explicit_actual_rank_matches(
                ctx,
                actual,
                actual_type.as_ref(),
                actual_rank,
                dummy_rank,
                assumed_rank,
                allows_sequence_association,
                dummy.type_info.as_ref(),
                symbol.attrs.elemental,
            ) {
                return false;
            }
        }
    }

    scope.arg_order.iter().enumerate().all(|(index, name)| {
        matched[index]
            || scope
                .symbols
                .get(name)
                .is_some_and(|dummy| dummy.attrs.optional)
    })
}

fn candidate_result_metadata(
    ctx: &Ctx<'_>,
    scope: &Scope,
    symbol: &Symbol,
    args: &[Argument],
    passed_object: Option<&SpannedExpr>,
) -> Option<(TypeInfo, usize)> {
    let result_symbol = result_symbol_in_procedure_scope(scope);
    let type_info = symbol
        .type_info
        .clone()
        .or_else(|| result_symbol.and_then(|result| result.type_info.clone()))?;
    let declared_rank = symbol_declared_result_rank(symbol).or_else(|| {
        result_symbol
            .map(|result| usize::from(result.attrs.result_rank).max(result.attrs.array_spec.len()))
    })?;
    let rank = if symbol.attrs.elemental && declared_rank == 0 {
        max_elemental_actual_rank(ctx, args)
            .into_iter()
            .chain(passed_object.and_then(|object| validation_expr_rank(ctx, object)))
            .max()
            .unwrap_or(0)
    } else {
        declared_rank
    };
    Some((type_info, rank))
}

fn named_generic_call_result_metadata(
    ctx: &Ctx<'_>,
    name: &str,
    args: &[Argument],
) -> Option<(TypeInfo, usize)> {
    let mut matches = Vec::new();
    for interface in ctx.lookup_lexical_named_interfaces(name) {
        for candidate_name in &interface.arg_names {
            let owner_scope = interface_specific_owner_scope(ctx, interface, candidate_name);
            let Some(scope) = assignment_candidate_scope(ctx, candidate_name, owner_scope) else {
                continue;
            };
            let Some(symbol) = candidate_symbol(ctx, candidate_name, owner_scope, scope) else {
                continue;
            };
            if !call_candidate_matches(ctx, scope, symbol, args, None) {
                continue;
            }
            if let Some(metadata) = candidate_result_metadata(ctx, scope, symbol, args, None) {
                matches.push(metadata);
            }
        }
    }
    let [metadata] = matches.as_slice() else {
        return None;
    };
    Some(metadata.clone())
}

fn operator_candidate_matches(
    ctx: &Ctx<'_>,
    scope: &Scope,
    symbol: &Symbol,
    operands: &[&SpannedExpr],
    operand_metadata: &[ValidationExprMetadata],
) -> bool {
    if operands.len() != operand_metadata.len() || operands.len() > scope.arg_order.len() {
        return false;
    }

    let mut elemental_rank = None;
    for (index, (actual, metadata)) in operands.iter().zip(operand_metadata).enumerate() {
        let Some(dummy) = scope
            .arg_order
            .get(index)
            .and_then(|name| scope.symbols.get(name))
        else {
            return false;
        };
        if let (Some(dummy_type), Some(actual_type)) =
            (dummy.type_info.as_ref(), metadata.type_info.as_ref())
        {
            if !generic_dummy_type_matches(ctx, scope.id, dummy_type, actual_type) {
                return false;
            }
        }
        if let Some(actual_rank) = metadata.rank {
            let dummy_rank = dummy.attrs.array_spec.len();
            if symbol.attrs.elemental && dummy_rank == 0 && actual_rank > 0 {
                if elemental_rank.is_some_and(|rank| rank != actual_rank) {
                    return false;
                }
                elemental_rank = Some(actual_rank);
            }
            let assumed_rank = dummy
                .attrs
                .array_spec
                .iter()
                .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedRank));
            let allows_sequence_association = !dummy.attrs.array_spec.is_empty()
                && dummy.attrs.array_spec.iter().all(|spec| {
                    matches!(
                        spec,
                        crate::ast::decl::ArraySpec::Explicit { .. }
                            | crate::ast::decl::ArraySpec::AssumedSize { .. }
                    )
                });
            if !explicit_actual_rank_matches(
                ctx,
                actual,
                metadata.type_info.as_ref(),
                actual_rank,
                dummy_rank,
                assumed_rank,
                allows_sequence_association,
                dummy.type_info.as_ref(),
                symbol.attrs.elemental,
            ) {
                return false;
            }
        }
    }

    scope.arg_order.iter().skip(operands.len()).all(|name| {
        scope
            .symbols
            .get(name)
            .is_some_and(|dummy| dummy.attrs.optional)
    })
}

fn operator_candidate_result_metadata(
    scope: &Scope,
    symbol: &Symbol,
    operand_metadata: &[ValidationExprMetadata],
) -> Option<(TypeInfo, usize)> {
    let result_symbol = result_symbol_in_procedure_scope(scope);
    let type_info = symbol
        .type_info
        .clone()
        .or_else(|| result_symbol.and_then(|result| result.type_info.clone()))?;
    let declared_rank = symbol_declared_result_rank(symbol).or_else(|| {
        result_symbol
            .map(|result| usize::from(result.attrs.result_rank).max(result.attrs.array_spec.len()))
    })?;
    let rank = if symbol.attrs.elemental && declared_rank == 0 {
        operand_metadata
            .iter()
            .filter_map(|metadata| metadata.rank)
            .max()
            .unwrap_or(0)
    } else {
        declared_rank
    };
    Some((type_info, rank))
}

fn defined_operator_result_metadata(
    ctx: &Ctx<'_>,
    interface_name: &str,
    operands: &[&SpannedExpr],
    operand_metadata: &[ValidationExprMetadata],
) -> Option<(TypeInfo, usize)> {
    let operand_types: Vec<&TypeInfo> = operand_metadata
        .iter()
        .map(|metadata| metadata.type_info.as_ref())
        .collect::<Option<_>>()?;
    let mut matches = Vec::new();
    for (name, owner_scope) in defined_interface_candidates(ctx, interface_name, &operand_types) {
        let Some(scope) = assignment_candidate_scope(ctx, &name, owner_scope) else {
            continue;
        };
        let Some(symbol) = candidate_symbol(ctx, &name, owner_scope, scope) else {
            continue;
        };
        if !operator_candidate_matches(ctx, scope, symbol, operands, operand_metadata) {
            continue;
        }
        if let Some(metadata) = operator_candidate_result_metadata(scope, symbol, operand_metadata)
        {
            matches.push(metadata);
        }
    }
    let [metadata] = matches.as_slice() else {
        return None;
    };
    Some(metadata.clone())
}

fn call_rank_argument_expr<'a>(
    args: &'a [Argument],
    position: usize,
    keywords: &[&str],
) -> Option<&'a SpannedExpr> {
    args.iter()
        .find(|arg| {
            arg.keyword.as_deref().is_some_and(|keyword| {
                keywords
                    .iter()
                    .any(|candidate| keyword.eq_ignore_ascii_case(candidate))
            })
        })
        .or_else(|| {
            args.iter()
                .filter(|arg| arg.keyword.is_none())
                .nth(position)
        })
        .and_then(|arg| match &arg.value {
            SectionSubscript::Element(expr) => Some(expr),
            SectionSubscript::Range { .. } => None,
        })
}

fn reduction_dim_argument_expr<'a>(
    ctx: &Ctx<'_>,
    args: &'a [Argument],
    position: usize,
) -> Option<&'a SpannedExpr> {
    if let Some(expr) = args
        .iter()
        .find(|arg| {
            arg.keyword
                .as_deref()
                .is_some_and(|keyword| keyword.eq_ignore_ascii_case("dim"))
        })
        .and_then(|arg| match &arg.value {
            SectionSubscript::Element(expr) => Some(expr),
            SectionSubscript::Range { .. } => None,
        })
    {
        return Some(expr);
    }

    let expr = args
        .iter()
        .filter(|arg| arg.keyword.is_none())
        .nth(position)
        .and_then(|arg| match &arg.value {
            SectionSubscript::Element(expr) => Some(expr),
            SectionSubscript::Range { .. } => None,
        })?;
    matches!(
        validation_expr_type_info(ctx, expr),
        Some(TypeInfo::Integer { .. })
    )
    .then_some(expr)
}

fn intrinsic_result_rank_is_scalar(name: &str) -> bool {
    matches!(
        name,
        "allocated"
            | "associated"
            | "bit_size"
            | "c_associated"
            | "c_funloc"
            | "c_loc"
            | "c_sizeof"
            | "command_argument_count"
            | "compiler_options"
            | "compiler_version"
            | "digits"
            | "dot_product"
            | "epsilon"
            | "f_c_string"
            | "huge"
            | "ieee_selected_real_kind"
            | "ieee_support_datatype"
            | "ieee_support_denormal"
            | "ieee_support_divide"
            | "ieee_support_flag"
            | "ieee_support_halting"
            | "ieee_support_inf"
            | "ieee_support_io"
            | "ieee_support_nan"
            | "ieee_support_rounding"
            | "ieee_support_sqrt"
            | "ieee_support_standard"
            | "ieee_support_subnormal"
            | "ieee_support_underflow_control"
            | "kind"
            | "len"
            | "maxexponent"
            | "minexponent"
            | "new_line"
            | "precision"
            | "present"
            | "radix"
            | "range"
            | "rank"
            | "repeat"
            | "same_type_as"
            | "selected_char_kind"
            | "selected_int_kind"
            | "selected_logical_kind"
            | "selected_real_kind"
            | "size"
            | "storage_size"
            | "tiny"
            | "trim"
    )
}

fn intrinsic_call_result_rank(ctx: &Ctx<'_>, name: &str, args: &[Argument]) -> Option<usize> {
    let key = name.to_ascii_lowercase();
    if crate::sema::types::is_elemental_intrinsic(&key) {
        return max_elemental_actual_rank(ctx, args).or(Some(0));
    }
    if intrinsic_result_rank_is_scalar(&key) {
        return Some(0);
    }

    let source_rank = |position, keywords: &[&str]| {
        call_rank_argument_expr(args, position, keywords)
            .and_then(|expr| validation_expr_rank(ctx, expr))
    };
    match key.as_str() {
        "reshape" => {
            let shape = call_rank_argument_expr(args, 1, &["shape"])?;
            match &shape.node {
                Expr::ArrayConstructor { values, .. } => Some(values.len()),
                _ => None,
            }
        }
        "transpose" => source_rank(0, &["matrix"]),
        "matmul" => {
            let lhs = source_rank(0, &["matrix_a"]);
            let rhs = source_rank(1, &["matrix_b"]);
            match (lhs, rhs) {
                (Some(2), Some(2)) => Some(2),
                (Some(2), Some(1)) | (Some(1), Some(2)) => Some(1),
                (Some(1), Some(1)) => Some(0),
                (Some(lhs), Some(rhs)) => Some(lhs.max(rhs).min(2)),
                (Some(rank), None) | (None, Some(rank)) => Some(rank.min(2)),
                (None, None) => None,
            }
        }
        "pack" => Some(1),
        "unpack" => source_rank(1, &["mask"]),
        "spread" => source_rank(0, &["source"]).map(|rank| rank + 1),
        "cshift" | "eoshift" => source_rank(0, &["array"]),
        "shape" => Some(1),
        "lbound" | "ubound" => {
            let has_dim = call_rank_argument_expr(args, 1, &["dim"]).is_some();
            Some(usize::from(!has_dim))
        }
        "all" | "any" | "count" | "norm2" => {
            let rank = source_rank(0, &["array", "mask"])?;
            let has_dim = call_rank_argument_expr(args, 1, &["dim"]).is_some();
            Some(if has_dim { rank.saturating_sub(1) } else { 0 })
        }
        "sum" | "product" | "maxval" | "minval" => {
            let rank = source_rank(0, &["array"])?;
            let has_dim = reduction_dim_argument_expr(ctx, args, 1).is_some();
            Some(if has_dim { rank.saturating_sub(1) } else { 0 })
        }
        "maxloc" | "minloc" => {
            let rank = source_rank(0, &["array"])?;
            let has_dim = reduction_dim_argument_expr(ctx, args, 1).is_some();
            Some(if has_dim { rank.saturating_sub(1) } else { 1 })
        }
        "findloc" => {
            let rank = source_rank(0, &["array"])?;
            let has_dim = reduction_dim_argument_expr(ctx, args, 2).is_some();
            Some(if has_dim { rank.saturating_sub(1) } else { 1 })
        }
        "merge" => max_elemental_actual_rank(ctx, args).or(Some(0)),
        "transfer" => {
            if call_rank_argument_expr(args, 2, &["size"]).is_some() {
                Some(1)
            } else {
                source_rank(1, &["mold"])
            }
        }
        _ => None,
    }
}

fn validation_expr_rank(ctx: &Ctx<'_>, expr: &SpannedExpr) -> Option<usize> {
    use crate::ast::expr::SectionSubscript;

    match &expr.node {
        Expr::Name { name } => ctx
            .block_binding_attrs(name)
            .and_then(|binding| binding.rank)
            .or_else(|| {
                ctx.lookup_lexical(name)
                    .map(|symbol| symbol.attrs.array_spec.len())
            }),
        Expr::ParenExpr { inner } => validation_expr_rank(ctx, inner),
        Expr::UnaryOp { .. } => validation_expr_metadata(ctx, expr).rank,
        Expr::ComponentAccess { base, .. } => {
            let base_rank = validation_expr_rank(ctx, base)?;
            let field_rank = leaf_field_layout(ctx, expr)?.field.dims.len();
            Some(base_rank + field_rank)
        }
        Expr::BinaryOp { .. } => validation_expr_metadata(ctx, expr).rank,
        Expr::ConditionalExpr { then_val, .. } => validation_expr_rank(ctx, then_val),
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                if let Some((_, rank)) = component_call_result_metadata(ctx, callee, args) {
                    return Some(rank);
                }
                let leaf = leaf_field_layout(ctx, expr)?;
                if leaf.field.procedure_pointer {
                    return None;
                }
                if matches!(leaf.field.type_info, TypeInfo::Character { .. })
                    && args
                        .iter()
                        .any(|arg| matches!(arg.value, SectionSubscript::Range { .. }))
                    && validation_expr_rank(ctx, callee) == Some(0)
                {
                    return Some(0);
                }
                if args.is_empty() {
                    return Some(leaf.field.dims.len());
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
            };
            let symbol = ctx.lookup_lexical(name);
            if symbol.is_some_and(|symbol| matches!(symbol.kind, SymbolKind::DerivedType)) {
                return Some(0);
            }
            if let Some((_, rank)) = named_generic_call_result_metadata(ctx, name, args) {
                return Some(rank);
            }
            if let Some((_, rank)) = symbol
                .and_then(|symbol| procedure_interface_call_result_metadata(ctx, symbol, args))
            {
                return Some(rank);
            }
            if let Some(symbol) = symbol {
                if matches!(symbol.kind, SymbolKind::Variable | SymbolKind::Parameter)
                    && !symbol.attrs.array_spec.is_empty()
                {
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
                let user_callable = matches!(
                    symbol.kind,
                    SymbolKind::Function
                        | SymbolKind::Subroutine
                        | SymbolKind::ExternalProc
                        | SymbolKind::ProcedurePointer
                        | SymbolKind::NamedInterface
                ) && !symbol.attrs.intrinsic;
                if is_intrinsic_name(name) && !user_callable {
                    if let Some(rank) = intrinsic_call_result_rank(ctx, name, args) {
                        return Some(rank);
                    }
                }
                if let Some(rank) = callable_invocation_rank(ctx, symbol, args) {
                    return Some(rank);
                }
            }
            is_intrinsic_name(name)
                .then(|| intrinsic_call_result_rank(ctx, name, args))
                .flatten()
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

fn assignment_resolves_defined_assignment(
    ctx: &Ctx<'_>,
    target: &SpannedExpr,
    value: &SpannedExpr,
) -> bool {
    let Some(lhs_type) = validation_expr_type_info(ctx, target) else {
        return false;
    };
    let Some(rhs_type) = validation_expr_type_info(ctx, value) else {
        return false;
    };

    let candidates = defined_interface_candidates(ctx, "assignment(=)", &[&lhs_type, &rhs_type]);

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
        let Some(target_rank) = validation_expr_rank(ctx, target) else {
            return false;
        };
        let Some(value_rank) = validation_expr_rank(ctx, value) else {
            return false;
        };
        let declared_ranks = [
            declared_args[0].attrs.array_spec.len(),
            declared_args[1].attrs.array_spec.len(),
        ];
        let elemental = candidate_symbol(ctx, &name, owner_scope, scope).is_some_and(|symbol| {
            matches!(symbol.kind, SymbolKind::Function | SymbolKind::Subroutine)
                && symbol.attrs.elemental
        });
        let ranks_match = if elemental && declared_ranks == [0, 0] {
            (target_rank == 0 && value_rank == 0)
                || (target_rank > 0 && (value_rank == 0 || target_rank == value_rank))
        } else {
            declared_ranks == [target_rank, value_rank]
        };
        if !ranks_match {
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

fn assignment_uses_defined_assignment(
    ctx: &Ctx<'_>,
    target: &SpannedExpr,
    value: &SpannedExpr,
) -> bool {
    validation_expr_rank(ctx, target) == Some(0)
        && validation_expr_rank(ctx, value) == Some(0)
        && assignment_resolves_defined_assignment(ctx, target, value)
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

struct MoveAllocCharacteristics {
    type_info: TypeInfo,
    rank: usize,
    polymorphic: bool,
}

fn move_alloc_characteristics(
    ctx: &mut Ctx<'_>,
    expr: &SpannedExpr,
    role: &str,
) -> Option<MoveAllocCharacteristics> {
    let (allocatable, type_info) = match &expr.node {
        Expr::Name { name } => {
            if ctx.is_associate_name(name) {
                (false, ctx.associate_binding_type_info(name).cloned())
            } else if let Some(binding) = ctx.block_binding_attrs(name) {
                (binding.allocatable, binding.type_info.clone())
            } else {
                let symbol = ctx.lookup_lexical(name)?;
                (
                    symbol.kind == SymbolKind::Variable && symbol.attrs.allocatable,
                    symbol.type_info.clone(),
                )
            }
        }
        Expr::ComponentAccess { base, .. } => {
            let leaf = leaf_field_layout(ctx, expr)?;
            let scalar_base = validation_expr_rank(ctx, base).is_none_or(|rank| rank == 0);
            (
                leaf.field.allocatable && scalar_base,
                Some(leaf.field.type_info.clone()),
            )
        }
        _ => (false, validation_expr_type_info(ctx, expr)),
    };

    let definable = actual_is_definable(ctx, expr, true).unwrap_or(true);
    if !allocatable || !definable {
        ctx.error(
            expr.span,
            format!("MOVE_ALLOC {role} argument must be a definable allocatable variable"),
        );
        return None;
    }

    let type_info = type_info?;
    let rank = validation_expr_rank(ctx, expr)?;
    let polymorphic = matches!(type_info, TypeInfo::Class(_) | TypeInfo::ClassStar);
    Some(MoveAllocCharacteristics {
        type_info,
        rank,
        polymorphic,
    })
}

fn move_alloc_nondeferred_type_parameters_match(source: &TypeInfo, target: &TypeInfo) -> bool {
    match (source, target) {
        (
            TypeInfo::Character {
                len: Some(source), ..
            },
            TypeInfo::Character {
                len: Some(target), ..
            },
        ) => source == target,
        _ => true,
    }
}

fn move_alloc_type_name(type_info: &TypeInfo) -> String {
    match type_info {
        TypeInfo::Character { len, kind } => format!(
            "CHARACTER(kind={},len={})",
            kind.unwrap_or(1),
            len.map_or_else(|| ":".to_string(), |len| len.to_string())
        ),
        _ => intrinsic_assignment_type_name(type_info),
    }
}

fn validate_move_alloc_arguments(ctx: &mut Ctx<'_>, args: &[crate::ast::expr::Argument]) {
    let Some(source) = call_argument_expr(args, 0, "from") else {
        return;
    };
    let Some(target) = call_argument_expr(args, 1, "to") else {
        return;
    };

    let source_characteristics = move_alloc_characteristics(ctx, source, "FROM");
    let target_characteristics = move_alloc_characteristics(ctx, target, "TO");

    if let (Some(source_info), Some(target_info)) =
        (&source_characteristics, &target_characteristics)
    {
        if source_info.rank != target_info.rank {
            ctx.error(
                target.span,
                format!(
                    "MOVE_ALLOC FROM and TO arguments must have the same rank (got rank-{} and rank-{})",
                    source_info.rank, target_info.rank
                ),
            );
        }
        if source_info.polymorphic && !target_info.polymorphic {
            ctx.error(
                target.span,
                "MOVE_ALLOC TO argument must be polymorphic when FROM is polymorphic",
            );
        } else if !generic_type_compatible(
            ctx,
            ctx.scope_id,
            &target_info.type_info,
            ctx.scope_id,
            &source_info.type_info,
        ) || !move_alloc_nondeferred_type_parameters_match(
            &source_info.type_info,
            &target_info.type_info,
        ) {
            ctx.error(
                target.span,
                format!(
                    "MOVE_ALLOC FROM and TO arguments must have compatible declared type and kind with matching nondeferred type parameters (got {} and {})",
                    move_alloc_type_name(&source_info.type_info),
                    move_alloc_type_name(&target_info.type_info)
                ),
            );
        }
    }

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

fn scope_has_private_component_access(ctx: &Ctx<'_>, owner_module: &str) -> bool {
    let mut scope_id = Some(ctx.lexical_scope_id());
    while let Some(current) = scope_id {
        let scope = ctx.st.scope(current);
        if matches!(
            &scope.kind,
            ScopeKind::Module(name) if name.eq_ignore_ascii_case(owner_module)
        ) || (matches!(scope.kind, ScopeKind::Submodule(_))
            && scope
                .submodule_ancestor
                .as_deref()
                .is_some_and(|ancestor| ancestor.eq_ignore_ascii_case(owner_module)))
        {
            return true;
        }
        scope_id = scope.parent;
    }
    false
}

fn field_is_accessible(ctx: &Ctx<'_>, field: &crate::sema::type_layout::FieldLayout) -> bool {
    if field.access != Access::Private {
        return true;
    }
    // PRIVATE components outside a module specification part are diagnosed
    // at their declarations. Avoid cascading an access error when such a
    // source-only layout has no meaningful module owner.
    field
        .owner_module
        .as_deref()
        .is_none_or(|owner| scope_has_private_component_access(ctx, owner))
}

fn report_inaccessible_component(
    ctx: &mut Ctx<'_>,
    span: Span,
    component: &str,
    base_type: &str,
    owner_module: Option<&str>,
) {
    let owner = owner_module
        .map(|module| format!(" (declared in module '{}')", module))
        .unwrap_or_default();
    ctx.error(
        span,
        format!(
            "private component '{}' of derived type '{}' is not accessible in this scope{}",
            component, base_type, owner
        ),
    );
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
    let Some(layout) = layouts
        .get_for_scope(ctx.lexical_scope_id(), &base_type)
        .or_else(|| layouts.get(&base_type))
    else {
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
    if let Expr::ComponentAccess { base, component } = &expr.node {
        if let (Some(base_type), Some(layouts)) =
            (derived_type_name_for_expr(ctx, base), ctx.type_layouts)
        {
            if let Some(layout) = layouts
                .get_for_scope(ctx.lexical_scope_id(), &base_type)
                .or_else(|| layouts.get(&base_type))
            {
                if let Some(field) = layout.field(component) {
                    if !field_is_accessible(ctx, field) {
                        report_inaccessible_component(
                            ctx,
                            expr.span,
                            component,
                            &base_type,
                            field.owner_module.as_deref(),
                        );
                    }
                }
            }
        }
    }
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

fn validate_structure_constructor_component_access(
    ctx: &mut Ctx<'_>,
    span: Span,
    type_name: &str,
    args: &[Argument],
) {
    let Some(layouts) = ctx.type_layouts else {
        return;
    };
    let Some(layout) = layouts
        .get_for_scope(ctx.lexical_scope_id(), type_name)
        .or_else(|| layouts.get(type_name))
    else {
        return;
    };

    let first_positional_type = args
        .first()
        .filter(|argument| argument.keyword.is_none())
        .and_then(|argument| match &argument.value {
            SectionSubscript::Element(value) => validation_expr_type_info(ctx, value),
            SectionSubscript::Range { .. } => None,
        });
    let positional_parent = crate::sema::type_layout::structure_constructor_uses_positional_parent(
        layout,
        layouts,
        first_positional_type.as_ref(),
    );
    let mut positional_index = 0usize;
    for argument in args {
        let field = crate::sema::type_layout::structure_constructor_field(
            layout,
            layouts,
            argument.keyword.as_deref(),
            positional_index,
            positional_parent,
        );
        if argument.keyword.is_none() {
            positional_index += 1;
        }
        let Some(field) = field else {
            continue;
        };
        let field = field.as_ref();
        if !field_is_accessible(ctx, field) {
            let argument_span = match &argument.value {
                SectionSubscript::Element(value) => value.span,
                SectionSubscript::Range { .. } => span,
            };
            report_inaccessible_component(
                ctx,
                argument_span,
                &field.name,
                type_name,
                field.owner_module.as_deref(),
            );
        }
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

/// Whether an I/O control list selects a character expression as its internal
/// file instead of an external unit.
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

/// F2018 RANDOM_INIT has two required scalar LOGICAL INTENT(IN)
/// arguments. Intrinsics have no user-declared explicit
/// interface to drive the generic call validator, so validate their
/// positional and keyword associations here.
fn validate_random_init_args(ctx: &mut Ctx, args: &[Argument], span: Span) {
    const FORMALS: [&str; 2] = ["repeatable", "image_distinct"];
    let mut associated = [false; FORMALS.len()];
    let mut positional = 0usize;
    let mut saw_keyword = false;

    for arg in args {
        let arg_span = argument_span(arg, span);
        let index = if let Some(keyword) = arg.keyword.as_deref() {
            saw_keyword = true;
            let Some(index) = FORMALS
                .iter()
                .position(|formal| formal.eq_ignore_ascii_case(keyword))
            else {
                ctx.error(
                    arg_span,
                    format!(
                        "unknown keyword argument '{}' in call to 'random_init'",
                        keyword
                    ),
                );
                continue;
            };
            index
        } else {
            if saw_keyword {
                ctx.error(
                    arg_span,
                    "positional argument follows a keyword argument in call to 'random_init'",
                );
                continue;
            }
            let index = positional;
            positional += 1;
            if index >= FORMALS.len() {
                continue;
            }
            index
        };

        if associated[index] {
            ctx.error(
                arg_span,
                format!(
                    "argument '{}' is associated more than once in call to 'random_init'",
                    FORMALS[index]
                ),
            );
            continue;
        }
        associated[index] = true;

        let SectionSubscript::Element(actual) = &arg.value else {
            ctx.error(
                arg_span,
                format!(
                    "RANDOM_INIT argument '{}' must be scalar",
                    FORMALS[index].to_ascii_uppercase()
                ),
            );
            continue;
        };
        if !matches!(
            validation_expr_type_info(ctx, actual),
            Some(TypeInfo::Logical { .. })
        ) {
            ctx.error(
                actual.span,
                format!(
                    "RANDOM_INIT argument '{}' must be LOGICAL",
                    FORMALS[index].to_ascii_uppercase()
                ),
            );
        }
        if validation_expr_rank(ctx, actual).is_some_and(|rank| rank != 0) {
            ctx.error(
                actual.span,
                format!(
                    "RANDOM_INIT argument '{}' must be scalar",
                    FORMALS[index].to_ascii_uppercase()
                ),
            );
        }
    }
}

fn validate_execute_command_line_cmdmsg(ctx: &mut Ctx<'_>, args: &[Argument]) {
    let Some(cmdmsg) = call_rank_argument_expr(args, 4, &["cmdmsg"]) else {
        return;
    };
    let is_scalar_character = matches!(
        validation_expr_type_info(ctx, cmdmsg),
        Some(TypeInfo::Character { .. })
    ) && validation_expr_rank(ctx, cmdmsg).is_none_or(|rank| rank == 0);
    let is_definable = actual_is_definable(ctx, cmdmsg, false).is_none_or(|value| value);
    if !is_scalar_character || !is_definable {
        ctx.error(
            cmdmsg.span,
            "EXECUTE_COMMAND_LINE CMDMSG must be a scalar CHARACTER variable",
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectProcedureKind {
    Function,
    Subroutine,
}

struct GenericSpecificCharacteristics {
    name: String,
    owner_scope: ScopeId,
    procedure: GenericProcedureCharacteristics,
}

struct GenericProcedureCharacteristics {
    kind: DirectProcedureKind,
    dummies: Vec<GenericDummyCharacteristics>,
    result: Option<GenericProcedureResult>,
    complete: bool,
}

struct GenericDummyCharacteristics {
    name: String,
    optional: bool,
    kind: GenericDummyKind,
}

enum GenericDummyKind {
    Data(GenericDataCharacteristics),
    Procedure(Option<Box<GenericProcedureCharacteristics>>),
}

struct GenericDataCharacteristics {
    type_info: Option<TypeInfo>,
    declared_scope: ScopeId,
    rank: usize,
    assumed_rank: bool,
    assumed_size: bool,
    allocatable: bool,
    pointer: bool,
    intent: Option<Intent>,
}

enum GenericProcedureResult {
    Data(GenericDataCharacteristics),
    Procedure(Option<Box<GenericProcedureCharacteristics>>),
}

fn generic_symbol_is_procedure_dummy(symbol: &Symbol) -> bool {
    symbol.attrs.external
        || symbol.attrs.procedure_iface.is_some()
        || matches!(
            symbol.kind,
            SymbolKind::Function
                | SymbolKind::Subroutine
                | SymbolKind::ExternalProc
                | SymbolKind::IntrinsicProc
                | SymbolKind::ProcedurePointer
        )
}

fn generic_data_characteristics(
    symbol: &Symbol,
    declared_scope: ScopeId,
) -> GenericDataCharacteristics {
    GenericDataCharacteristics {
        type_info: symbol.type_info.clone(),
        declared_scope,
        rank: symbol.attrs.array_spec.len(),
        assumed_rank: symbol
            .attrs
            .array_spec
            .iter()
            .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedRank)),
        assumed_size: symbol
            .attrs
            .array_spec
            .iter()
            .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedSize { .. })),
        allocatable: symbol.attrs.allocatable,
        pointer: symbol.attrs.pointer,
        intent: symbol.attrs.intent,
    }
}

fn generic_nested_procedure_characteristics(
    ctx: &Ctx<'_>,
    symbol: &Symbol,
    visiting: &mut HashSet<ScopeId>,
) -> Option<Box<GenericProcedureCharacteristics>> {
    let interface_name = symbol.attrs.procedure_iface.as_deref()?;
    let interface_symbol = ctx.st.lookup_in(symbol.scope, interface_name)?;
    let procedure_scope =
        assignment_candidate_scope(ctx, &interface_symbol.name, interface_symbol.scope)?;
    build_generic_procedure_characteristics(ctx, interface_symbol, procedure_scope, visiting)
        .map(Box::new)
}

fn generic_function_result(
    ctx: &Ctx<'_>,
    symbol: &Symbol,
    procedure_scope: &Scope,
    declared_scope: ScopeId,
    visiting: &mut HashSet<ScopeId>,
) -> Option<GenericProcedureResult> {
    let result_symbol = procedure_scope.procedure_result_symbol();
    if let Some(result) = result_symbol.filter(|result| {
        result.attrs.procedure_iface.is_some()
            || matches!(result.kind, SymbolKind::ProcedurePointer)
    }) {
        return Some(GenericProcedureResult::Procedure(
            generic_nested_procedure_characteristics(ctx, result, visiting),
        ));
    }

    let type_info = symbol
        .type_info
        .clone()
        .or_else(|| result_symbol.and_then(|result| result.type_info.clone()))?;
    let metadata_symbol = result_symbol.unwrap_or(symbol);
    let mut data = generic_data_characteristics(metadata_symbol, declared_scope);
    data.type_info = Some(type_info);
    data.rank = symbol_declared_result_rank(symbol)
        .unwrap_or(0)
        .max(data.rank)
        .max(usize::from(metadata_symbol.attrs.result_rank));
    Some(GenericProcedureResult::Data(data))
}

fn build_generic_procedure_characteristics(
    ctx: &Ctx<'_>,
    symbol: &Symbol,
    procedure_scope: &Scope,
    visiting: &mut HashSet<ScopeId>,
) -> Option<GenericProcedureCharacteristics> {
    if !visiting.insert(procedure_scope.id) {
        return None;
    }
    let characteristics = (|| {
        let kind = match procedure_scope.kind {
            ScopeKind::Function(_) => DirectProcedureKind::Function,
            ScopeKind::Subroutine(_) => DirectProcedureKind::Subroutine,
            _ => return None,
        };
        let declared_scope = ctx
            .type_layouts
            .filter(|layouts| layouts.scope_path(procedure_scope.id).is_some())
            .map_or(symbol.scope, |_| procedure_scope.id);
        let mut complete = true;
        let mut dummies = Vec::with_capacity(procedure_scope.arg_order.len());
        for dummy_name in &procedure_scope.arg_order {
            let dummy = procedure_scope
                .symbols
                .get(&dummy_name.to_ascii_lowercase())?;
            let dummy_kind = if generic_symbol_is_procedure_dummy(dummy) {
                let nested = generic_nested_procedure_characteristics(ctx, dummy, visiting);
                complete &= nested
                    .as_deref()
                    .is_some_and(|characteristics| characteristics.complete);
                GenericDummyKind::Procedure(nested)
            } else {
                let data = generic_data_characteristics(dummy, declared_scope);
                complete &= data.type_info.is_some();
                GenericDummyKind::Data(data)
            };
            dummies.push(GenericDummyCharacteristics {
                name: dummy.name.to_ascii_lowercase(),
                optional: dummy.attrs.optional,
                kind: dummy_kind,
            });
        }
        let result = (kind == DirectProcedureKind::Function)
            .then(|| {
                generic_function_result(ctx, symbol, procedure_scope, declared_scope, visiting)
            })
            .flatten();
        Some(GenericProcedureCharacteristics {
            kind,
            dummies,
            result,
            complete,
        })
    })();
    visiting.remove(&procedure_scope.id);
    characteristics
}

fn generic_same_derived_type(
    ctx: &Ctx<'_>,
    left_scope: ScopeId,
    left: &str,
    right_scope: ScopeId,
    right: &str,
) -> bool {
    let Some(layouts) = ctx.type_layouts else {
        return left.eq_ignore_ascii_case(right);
    };
    let left_layout = layouts
        .get_for_scope(left_scope, left)
        .or_else(|| layouts.get(left));
    let right_layout = layouts
        .get_for_scope(right_scope, right)
        .or_else(|| layouts.get(right));
    match (left_layout, right_layout) {
        (Some(left), Some(right)) => {
            layouts.canonical_key_for_layout(left) == layouts.canonical_key_for_layout(right)
        }
        _ => left.eq_ignore_ascii_case(right),
    }
}

fn generic_type_is_same_or_extension(
    ctx: &Ctx<'_>,
    actual_scope: ScopeId,
    actual: &str,
    declared_scope: ScopeId,
    declared: &str,
) -> bool {
    let Some(layouts) = ctx.type_layouts else {
        return actual.eq_ignore_ascii_case(declared);
    };
    let actual_layout = layouts
        .get_for_scope(actual_scope, actual)
        .or_else(|| layouts.get(actual));
    let declared_layout = layouts
        .get_for_scope(declared_scope, declared)
        .or_else(|| layouts.get(declared));
    match (actual_layout, declared_layout) {
        (Some(actual), Some(declared)) => layouts.is_same_or_extension_of(actual, declared),
        _ => actual.eq_ignore_ascii_case(declared),
    }
}

fn generic_same_enumeration(
    ctx: &Ctx<'_>,
    left_scope: ScopeId,
    left: &str,
    right_scope: ScopeId,
    right: &str,
) -> bool {
    if !left.eq_ignore_ascii_case(right) {
        return false;
    }
    match (
        ctx.st.lookup_in(left_scope, left),
        ctx.st.lookup_in(right_scope, right),
    ) {
        (Some(left), Some(right)) => {
            left.kind == SymbolKind::EnumerationType
                && right.kind == SymbolKind::EnumerationType
                && left.scope == right.scope
        }
        _ => true,
    }
}

fn generic_type_compatible(
    ctx: &Ctx<'_>,
    left_scope: ScopeId,
    left: &TypeInfo,
    right_scope: ScopeId,
    right: &TypeInfo,
) -> bool {
    fn kind_eq(left: Option<u8>, right: Option<u8>, default: u8) -> bool {
        left.unwrap_or(default) == right.unwrap_or(default)
    }

    match (left, right) {
        // Type compatibility is directional (F2023 7.3.3). An unlimited
        // polymorphic declared type accepts every dynamic type, but the
        // reverse is not true. Assumed type is TK-compatible only with
        // assumed type; its permissive actual-argument rules are separate.
        (TypeInfo::ClassStar, _) => true,
        (_, TypeInfo::ClassStar) => false,
        (TypeInfo::TypeStar, TypeInfo::TypeStar) => true,
        (TypeInfo::TypeStar, _) | (_, TypeInfo::TypeStar) => false,
        (TypeInfo::Derived(left), TypeInfo::Derived(right)) => {
            generic_same_derived_type(ctx, left_scope, left, right_scope, right)
        }
        (TypeInfo::Class(left), TypeInfo::Derived(right) | TypeInfo::Class(right)) => {
            generic_type_is_same_or_extension(ctx, right_scope, right, left_scope, left)
        }
        (TypeInfo::Derived(left), TypeInfo::Class(right)) => {
            generic_same_derived_type(ctx, left_scope, left, right_scope, right)
        }
        (TypeInfo::Integer { kind: left }, TypeInfo::Integer { kind: right }) => {
            kind_eq(*left, *right, crate::driver::defaults::default_int_kind())
        }
        (TypeInfo::Real { kind: left }, TypeInfo::Real { kind: right }) => {
            kind_eq(*left, *right, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::DoublePrecision, TypeInfo::DoublePrecision) => true,
        (TypeInfo::DoublePrecision, TypeInfo::Real { kind })
        | (TypeInfo::Real { kind }, TypeInfo::DoublePrecision) => {
            kind_eq(Some(8), *kind, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::Complex { kind: left }, TypeInfo::Complex { kind: right }) => {
            kind_eq(*left, *right, crate::driver::defaults::default_real_kind())
        }
        (TypeInfo::Logical { kind: left }, TypeInfo::Logical { kind: right }) => {
            kind_eq(*left, *right, crate::driver::defaults::default_int_kind())
        }
        (
            TypeInfo::Character {
                kind: left_kind, ..
            },
            TypeInfo::Character {
                kind: right_kind, ..
            },
        ) => kind_eq(*left_kind, *right_kind, 1),
        (TypeInfo::Enumeration(left), TypeInfo::Enumeration(right)) => {
            generic_same_enumeration(ctx, left_scope, left, right_scope, right)
        }
        _ => false,
    }
}

fn generic_data_tkr_compatible(
    ctx: &Ctx<'_>,
    left: &GenericDataCharacteristics,
    right: &GenericDataCharacteristics,
) -> bool {
    let types_compatible = match (&left.type_info, &right.type_info) {
        (Some(left_type), Some(right_type)) => generic_type_compatible(
            ctx,
            left.declared_scope,
            left_type,
            right.declared_scope,
            right_type,
        ),
        _ => true,
    };
    if !types_compatible {
        return false;
    }
    left.assumed_rank || right.assumed_rank || left.rank == right.rank
}

fn generic_data_distinguishable(
    ctx: &Ctx<'_>,
    left: &GenericDataCharacteristics,
    right: &GenericDataCharacteristics,
) -> bool {
    let types_overlap = match (&left.type_info, &right.type_info) {
        (Some(left_type), Some(right_type)) => {
            generic_type_compatible(
                ctx,
                left.declared_scope,
                left_type,
                right.declared_scope,
                right_type,
            ) || generic_type_compatible(
                ctx,
                right.declared_scope,
                right_type,
                left.declared_scope,
                left_type,
            )
        }
        _ => true,
    };
    if !types_overlap {
        return true;
    }
    let assumed_type_scalar_exception = (left.assumed_size
        && matches!(left.type_info, Some(TypeInfo::TypeStar))
        && right.rank == 0)
        || (right.assumed_size
            && matches!(right.type_info, Some(TypeInfo::TypeStar))
            && left.rank == 0);
    if !left.assumed_rank
        && !right.assumed_rank
        && !assumed_type_scalar_exception
        && left.rank != right.rank
    {
        return true;
    }
    (left.allocatable && right.pointer && right.intent != Some(Intent::In))
        || (right.allocatable && left.pointer && left.intent != Some(Intent::In))
}

fn generic_results_distinguishable(
    ctx: &Ctx<'_>,
    left: &GenericProcedureResult,
    right: &GenericProcedureResult,
) -> bool {
    match (left, right) {
        (GenericProcedureResult::Data(left), GenericProcedureResult::Data(right)) => {
            generic_data_distinguishable(ctx, left, right)
        }
        (GenericProcedureResult::Procedure(left), GenericProcedureResult::Procedure(right)) => {
            match (left.as_deref(), right.as_deref()) {
                (Some(left), Some(right)) => {
                    generic_procedures_distinguishable(ctx, left, right).unwrap_or(false)
                }
                _ => false,
            }
        }
        _ => true,
    }
}

fn generic_dummy_distinguishable(
    ctx: &Ctx<'_>,
    left: &GenericDummyCharacteristics,
    right: &GenericDummyCharacteristics,
) -> bool {
    match (&left.kind, &right.kind) {
        (GenericDummyKind::Data(left), GenericDummyKind::Data(right)) => {
            generic_data_distinguishable(ctx, left, right)
        }
        (GenericDummyKind::Procedure(left), GenericDummyKind::Procedure(right)) => {
            let (Some(left), Some(right)) = (left.as_deref(), right.as_deref()) else {
                return false;
            };
            if generic_procedures_distinguishable(ctx, left, right).unwrap_or(false) {
                return true;
            }
            match (&left.result, &right.result) {
                (Some(left), Some(right)) => generic_results_distinguishable(ctx, left, right),
                _ => false,
            }
        }
        _ => true,
    }
}

fn generic_rule_one_distinguishes(
    ctx: &Ctx<'_>,
    left: &[GenericDummyCharacteristics],
    right: &[GenericDummyCharacteristics],
) -> bool {
    left.iter().chain(right).any(|candidate| {
        let GenericDummyKind::Data(candidate_data) = &candidate.kind else {
            return false;
        };
        let compatible = |arguments: &[GenericDummyCharacteristics]| {
            arguments
                .iter()
                .filter(|argument| !argument.optional)
                .filter_map(|argument| match &argument.kind {
                    GenericDummyKind::Data(data) => Some(data),
                    GenericDummyKind::Procedure(_) => None,
                })
                .filter(|data| generic_data_tkr_compatible(ctx, candidate_data, data))
                .count()
        };
        let not_distinguishable = |arguments: &[GenericDummyCharacteristics]| {
            arguments
                .iter()
                .filter(|argument| {
                    matches!(argument.kind, GenericDummyKind::Data(_))
                        && !generic_dummy_distinguishable(ctx, candidate, argument)
                })
                .count()
        };
        compatible(left) > not_distinguishable(right)
            || compatible(right) > not_distinguishable(left)
    })
}

fn generic_first_position_disambiguator(
    ctx: &Ctx<'_>,
    left: &[GenericDummyCharacteristics],
    right: &[GenericDummyCharacteristics],
) -> Option<usize> {
    left.iter().enumerate().find_map(|(index, argument)| {
        if argument.optional {
            return None;
        }
        right
            .get(index)
            .is_none_or(|other| generic_dummy_distinguishable(ctx, argument, other))
            .then_some(index)
    })
}

fn generic_last_name_disambiguator(
    ctx: &Ctx<'_>,
    left: &[GenericDummyCharacteristics],
    right: &[GenericDummyCharacteristics],
) -> Option<usize> {
    left.iter().enumerate().rev().find_map(|(index, argument)| {
        if argument.optional {
            return None;
        }
        right
            .iter()
            .find(|other| other.name == argument.name)
            .is_none_or(|other| generic_dummy_distinguishable(ctx, argument, other))
            .then_some(index)
    })
}

fn generic_rule_four_distinguishes(
    ctx: &Ctx<'_>,
    left: &[GenericDummyCharacteristics],
    right: &[GenericDummyCharacteristics],
) -> bool {
    let one_way = |first: &[GenericDummyCharacteristics],
                   second: &[GenericDummyCharacteristics]| {
        generic_first_position_disambiguator(ctx, first, second)
            .zip(generic_last_name_disambiguator(ctx, first, second))
            .is_some_and(|(position, name)| position <= name)
    };
    one_way(left, right) || one_way(right, left)
}

fn generic_procedures_distinguishable(
    ctx: &Ctx<'_>,
    left: &GenericProcedureCharacteristics,
    right: &GenericProcedureCharacteristics,
) -> Option<bool> {
    if left.kind != right.kind {
        return Some(true);
    }

    let procedure_counts = |arguments: &[GenericDummyCharacteristics]| {
        arguments.iter().fold((0usize, 0usize), |counts, argument| {
            if matches!(argument.kind, GenericDummyKind::Procedure(_)) {
                (counts.0 + 1, counts.1 + usize::from(!argument.optional))
            } else {
                counts
            }
        })
    };
    let (left_procedures, left_required_procedures) = procedure_counts(&left.dummies);
    let (right_procedures, right_required_procedures) = procedure_counts(&right.dummies);
    if left_required_procedures > right_procedures
        || right_required_procedures > left_procedures
        || generic_rule_one_distinguishes(ctx, &left.dummies, &right.dummies)
        || generic_rule_four_distinguishes(ctx, &left.dummies, &right.dummies)
    {
        return Some(true);
    }

    if !left.complete || !right.complete {
        return None;
    }
    let has_optional_or_unlimited_data = |procedure: &GenericProcedureCharacteristics| {
        procedure
            .dummies
            .iter()
            .any(|argument| match &argument.kind {
                GenericDummyKind::Data(data) => {
                    argument.optional || matches!(data.type_info, Some(TypeInfo::ClassStar))
                }
                GenericDummyKind::Procedure(_) => false,
            })
    };
    if has_optional_or_unlimited_data(left) && has_optional_or_unlimited_data(right) {
        None
    } else {
        Some(false)
    }
}

struct IndistinguishableGenericPair {
    left_name: String,
    left_owner_scope: ScopeId,
    right_name: String,
    right_owner_scope: ScopeId,
}

fn indistinguishable_generic_specifics_from_interfaces(
    ctx: &Ctx<'_>,
    interfaces: &[&Symbol],
) -> Option<IndistinguishableGenericPair> {
    let mut specifics = Vec::new();
    let mut seen = HashSet::new();
    for interface in interfaces {
        for specific_name in &interface.arg_names {
            let owner_scope =
                interface_specific_owner_scope(ctx, interface, specific_name.as_str());
            let Some(procedure_scope) = assignment_candidate_scope(ctx, specific_name, owner_scope)
            else {
                continue;
            };
            let key = (owner_scope, specific_name.to_ascii_lowercase());
            if !seen.insert(key) {
                continue;
            }
            let Some(symbol) = candidate_symbol(ctx, specific_name, owner_scope, procedure_scope)
            else {
                continue;
            };
            let mut visiting = HashSet::new();
            let Some(procedure) = build_generic_procedure_characteristics(
                ctx,
                symbol,
                procedure_scope,
                &mut visiting,
            ) else {
                continue;
            };
            specifics.push(GenericSpecificCharacteristics {
                name: specific_name.to_ascii_lowercase(),
                owner_scope,
                procedure,
            });
        }
    }
    specifics.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.owner_scope.cmp(&right.owner_scope))
    });

    for left_index in 0..specifics.len() {
        for right in &specifics[left_index + 1..] {
            let left = &specifics[left_index];
            if left.owner_scope == right.owner_scope {
                continue;
            }
            if generic_procedures_distinguishable(ctx, &left.procedure, &right.procedure)
                == Some(false)
            {
                return Some(IndistinguishableGenericPair {
                    left_name: left.name.clone(),
                    left_owner_scope: left.owner_scope,
                    right_name: right.name.clone(),
                    right_owner_scope: right.owner_scope,
                });
            }
        }
    }
    None
}

fn indistinguishable_merged_generic_specifics(
    ctx: &Ctx<'_>,
    generic_name: &str,
) -> Option<IndistinguishableGenericPair> {
    let interfaces = ctx.lookup_lexical_named_interfaces(generic_name);
    indistinguishable_generic_specifics_from_interfaces(ctx, &interfaces)
}

enum ExplicitInterfaceResolution {
    None,
    Resolved(ExplicitProcedureInterface),
    Invalid(String),
}

struct ExplicitDummyArg {
    name: String,
    type_info: Option<TypeInfo>,
    intent: Option<Intent>,
    optional: bool,
    allocatable: bool,
    pointer: bool,
    procedure: bool,
    procedure_scope: ScopeId,
    procedure_has_explicit_interface: bool,
    rank: usize,
    assumed_rank: bool,
    allows_sequence_association: bool,
}

struct ExplicitProcedureInterface {
    name: String,
    declared_scope: ScopeId,
    elemental: bool,
    dummies: Vec<ExplicitDummyArg>,
}

fn procedure_scope_kind_matches(scope: &Scope, expected_kind: DirectProcedureKind) -> bool {
    matches!(
        (&scope.kind, expected_kind),
        (ScopeKind::Function(_), DirectProcedureKind::Function)
            | (ScopeKind::Subroutine(_), DirectProcedureKind::Subroutine)
    )
}

fn build_explicit_procedure_interface(
    ctx: &Ctx<'_>,
    call_name: &str,
    symbol: &Symbol,
    procedure_scope: &Scope,
) -> Option<ExplicitProcedureInterface> {
    let mut dummies = Vec::with_capacity(procedure_scope.arg_order.len());
    for dummy_name in &procedure_scope.arg_order {
        let dummy = procedure_scope
            .symbols
            .get(&dummy_name.to_ascii_lowercase())?;
        let assumed_rank = dummy
            .attrs
            .array_spec
            .iter()
            .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedRank));
        let allows_sequence_association = !dummy.attrs.array_spec.is_empty()
            && dummy.attrs.array_spec.iter().all(|spec| {
                matches!(
                    spec,
                    crate::ast::decl::ArraySpec::Explicit { .. }
                        | crate::ast::decl::ArraySpec::AssumedSize { .. }
                )
            });
        dummies.push(ExplicitDummyArg {
            name: dummy.name.clone(),
            type_info: dummy.type_info.clone(),
            intent: dummy.attrs.intent,
            optional: dummy.attrs.optional,
            allocatable: dummy.attrs.allocatable,
            pointer: dummy.attrs.pointer,
            procedure: dummy.attrs.external || dummy.attrs.procedure_iface.is_some(),
            procedure_scope: procedure_scope.id,
            procedure_has_explicit_interface: dummy.attrs.procedure_iface.is_some(),
            rank: dummy.attrs.array_spec.len(),
            assumed_rank,
            allows_sequence_association,
        });
    }

    let declared_scope = ctx
        .type_layouts
        .filter(|layouts| layouts.scope_path(procedure_scope.id).is_some())
        .map_or(symbol.scope, |_| procedure_scope.id);

    Some(ExplicitProcedureInterface {
        name: call_name.to_string(),
        declared_scope,
        elemental: symbol.attrs.elemental,
        dummies,
    })
}

fn resolve_generic_procedure_interface(
    ctx: &Ctx<'_>,
    name: &str,
    args: &[Argument],
    expected_kind: DirectProcedureKind,
    interfaces: &[&Symbol],
) -> ExplicitInterfaceResolution {
    let mut candidates = Vec::new();
    let mut seen_scopes = HashSet::new();
    for interface in interfaces {
        for candidate_name in &interface.arg_names {
            let owner_scope = interface_specific_owner_scope(ctx, interface, candidate_name);
            let Some(scope) = assignment_candidate_scope(ctx, candidate_name, owner_scope) else {
                continue;
            };
            if !procedure_scope_kind_matches(scope, expected_kind) || !seen_scopes.insert(scope.id)
            {
                continue;
            }
            let Some(symbol) = candidate_symbol(ctx, candidate_name, owner_scope, scope) else {
                continue;
            };
            candidates.push((scope, symbol));
        }
    }

    let matches: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|(scope, symbol)| call_candidate_matches(ctx, scope, symbol, args, None))
        .collect();
    let [(scope, symbol)] = matches.as_slice() else {
        return ExplicitInterfaceResolution::None;
    };
    build_explicit_procedure_interface(ctx, name, symbol, scope).map_or(
        ExplicitInterfaceResolution::None,
        ExplicitInterfaceResolution::Resolved,
    )
}

fn direct_procedure_interface(
    ctx: &Ctx<'_>,
    callee: &SpannedExpr,
    args: &[Argument],
    expected_kind: DirectProcedureKind,
) -> ExplicitInterfaceResolution {
    let Expr::Name { name } = &callee.node else {
        return ExplicitInterfaceResolution::None;
    };
    let interfaces = ctx.lookup_lexical_named_interfaces(name);
    if !interfaces.is_empty() {
        return resolve_generic_procedure_interface(ctx, name, args, expected_kind, &interfaces);
    }

    let Some(symbol) = ctx.lookup_lexical(name) else {
        return ExplicitInterfaceResolution::None;
    };
    if let Some(interface_name) = symbol.attrs.procedure_iface.as_deref() {
        let interface_symbol = ctx
            .st
            .lookup_in(symbol.scope, interface_name)
            .or_else(|| ctx.lookup_lexical(interface_name));
        let Some(interface_symbol) = interface_symbol else {
            return ExplicitInterfaceResolution::Invalid(format!(
                "explicit interface '{}' for procedure '{}' is unavailable",
                interface_name, name
            ));
        };
        let Some(procedure_scope) =
            assignment_candidate_scope(ctx, &interface_symbol.name, interface_symbol.scope)
        else {
            return ExplicitInterfaceResolution::Invalid(format!(
                "explicit interface '{}' for procedure '{}' has no procedure signature",
                interface_name, name
            ));
        };
        if !procedure_scope_kind_matches(procedure_scope, expected_kind) {
            let expected = match expected_kind {
                DirectProcedureKind::Function => "function",
                DirectProcedureKind::Subroutine => "subroutine",
            };
            return ExplicitInterfaceResolution::Invalid(format!(
                "procedure '{}' does not have a {} interface",
                name, expected
            ));
        }
        return build_explicit_procedure_interface(ctx, name, interface_symbol, procedure_scope)
            .map_or(
                ExplicitInterfaceResolution::None,
                ExplicitInterfaceResolution::Resolved,
            );
    }

    let kind_matches = matches!(
        (&symbol.kind, expected_kind),
        (SymbolKind::Function, DirectProcedureKind::Function)
            | (SymbolKind::Subroutine, DirectProcedureKind::Subroutine)
    );
    if !kind_matches || symbol.attrs.intrinsic {
        return ExplicitInterfaceResolution::None;
    }

    let Some(procedure_scope) = assignment_candidate_scope(ctx, &symbol.name, symbol.scope) else {
        return ExplicitInterfaceResolution::None;
    };
    let interface_parent = procedure_scope
        .parent
        .is_some_and(|parent| matches!(ctx.st.scope(parent).kind, ScopeKind::Interface));
    if matches!(ctx.st.scope(symbol.scope).kind, ScopeKind::Global) && !interface_parent {
        // A separately defined external procedure still has an implicit
        // interface unless the caller declared an interface body for it.
        return ExplicitInterfaceResolution::None;
    }

    build_explicit_procedure_interface(ctx, name, symbol, procedure_scope).map_or(
        ExplicitInterfaceResolution::None,
        ExplicitInterfaceResolution::Resolved,
    )
}

fn argument_span(arg: &Argument, fallback: Span) -> Span {
    match &arg.value {
        SectionSubscript::Element(expr) => expr.span,
        SectionSubscript::Range { start, end, stride } => start
            .as_ref()
            .map(|expr| expr.span)
            .or_else(|| end.as_ref().map(|expr| expr.span))
            .or_else(|| stride.as_ref().map(|expr| expr.span))
            .unwrap_or(fallback),
    }
}

fn is_array_element_designator(ctx: &Ctx<'_>, actual: &SpannedExpr) -> bool {
    let Expr::FunctionCall { callee, args } = &actual.node else {
        return false;
    };
    args.iter()
        .all(|arg| matches!(arg.value, SectionSubscript::Element(_)))
        && validation_expr_rank(ctx, callee).is_some_and(|rank| rank > 0)
}

fn named_actual_is_definable(
    ctx: &Ctx<'_>,
    name: &str,
    path_has_pointer: bool,
    defines_association: bool,
) -> Option<bool> {
    if ctx.is_associate_name(name) {
        return None;
    }
    if let Some(binding) = ctx.block_binding_attrs(name) {
        let defines_pointer_target = !defines_association && (path_has_pointer || binding.pointer);
        return Some(!binding.parameter && (!binding.intent_in || defines_pointer_target));
    }
    let symbol = ctx.lookup_lexical(name)?;
    if !matches!(symbol.kind, SymbolKind::Variable | SymbolKind::Parameter) {
        return Some(false);
    }
    Some(
        !symbol.attrs.parameter
            && (!matches!(symbol.attrs.intent, Some(Intent::In))
                || (!defines_association && (path_has_pointer || symbol.attrs.pointer))),
    )
}

fn actual_is_definable(
    ctx: &Ctx<'_>,
    actual: &SpannedExpr,
    defines_association: bool,
) -> Option<bool> {
    match &actual.node {
        Expr::Name { name } => named_actual_is_definable(ctx, name, false, defines_association),
        Expr::ComponentAccess { .. } => {
            let base = extract_base_name(actual)?;
            let through_pointer = leaf_field_layout(ctx, actual)
                .is_some_and(|leaf| leaf.field.pointer || leaf.ancestor_is_pointer);
            named_actual_is_definable(ctx, &base, through_pointer, defines_association)
        }
        Expr::FunctionCall { callee, args } => {
            let has_vector_subscript = args.iter().any(|arg| match &arg.value {
                SectionSubscript::Element(index) => {
                    validation_expr_rank(ctx, index).is_some_and(|rank| rank > 0)
                }
                SectionSubscript::Range { .. } => false,
            });
            match &callee.node {
                Expr::Name { name } => {
                    let symbol = ctx.lookup_lexical(name)?;
                    match symbol.kind {
                        SymbolKind::Variable | SymbolKind::Parameter => {
                            if has_vector_subscript {
                                return Some(false);
                            }
                            named_actual_is_definable(ctx, name, false, defines_association)
                        }
                        SymbolKind::Function => Some(symbol.attrs.pointer && !defines_association),
                        _ => Some(false),
                    }
                }
                Expr::ComponentAccess { .. } => {
                    let leaf = leaf_field_layout(ctx, callee)?;
                    if leaf.field.procedure_pointer {
                        return None;
                    }
                    if has_vector_subscript {
                        return Some(false);
                    }
                    let base = extract_base_name(callee)?;
                    named_actual_is_definable(
                        ctx,
                        &base,
                        leaf.field.pointer || leaf.ancestor_is_pointer,
                        defines_association,
                    )
                }
                _ => Some(false),
            }
        }
        Expr::NilArgument => None,
        Expr::ConditionalExpr {
            then_val, else_val, ..
        } => {
            let then_definable = actual_is_definable(ctx, then_val, defines_association);
            let else_definable = actual_is_definable(ctx, else_val, defines_association);
            match (then_definable, else_definable) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), _) | (_, Some(true)) => Some(true),
                (None, None) => None,
            }
        }
        Expr::ParenExpr { .. }
        | Expr::IntegerLiteral { .. }
        | Expr::RealLiteral { .. }
        | Expr::StringLiteral { .. }
        | Expr::LogicalLiteral { .. }
        | Expr::ComplexLiteral { .. }
        | Expr::BozLiteral { .. }
        | Expr::UnaryOp { .. }
        | Expr::BinaryOp { .. }
        | Expr::ArrayConstructor { .. } => Some(false),
    }
}

fn validate_explicit_actual(
    ctx: &mut Ctx,
    interface: &ExplicitProcedureInterface,
    dummy: &ExplicitDummyArg,
    actual: &SpannedExpr,
    elemental_rank: &mut Option<usize>,
) {
    if expr_has_nil_arm(actual) && !dummy.optional {
        ctx.error(
            actual.span,
            format!(
                "dummy argument '{}' is not OPTIONAL (F2023 C1525)",
                dummy.name
            ),
        );
    }
    if matches!(actual.node, Expr::NilArgument) {
        return;
    }
    if dummy.procedure {
        validate_procedure_dummy_actual(
            ctx,
            &dummy.name,
            dummy.procedure_scope,
            dummy.procedure_has_explicit_interface,
            dummy.pointer,
            dummy.intent,
            actual,
        );
        return;
    }

    if let (Some(dummy_type), Some(actual_type)) = (
        dummy.type_info.as_ref(),
        validation_expr_type_info(ctx, actual).as_ref(),
    ) {
        match (dummy_type, actual_type) {
            (TypeInfo::Enumeration(expected), TypeInfo::Enumeration(actual_name)) => {
                if !expected.eq_ignore_ascii_case(actual_name) {
                    ctx.error(
                        actual.span,
                        format!(
                            "actual argument of enumeration type '{}' is not compatible with dummy '{}' of enumeration type '{}'",
                            actual_name, dummy.name, expected
                        ),
                    );
                }
            }
            (TypeInfo::Enumeration(expected), _) => {
                ctx.error(
                    actual.span,
                    format!(
                        "dummy argument '{}' has enumeration type '{}'; pass a value of that type (constructor '{}(int-expr)')",
                        dummy.name, expected, expected
                    ),
                );
            }
            (TypeInfo::ClassStar | TypeInfo::TypeStar, TypeInfo::Enumeration(_)) => {}
            (_, TypeInfo::Enumeration(actual_name)) => {
                ctx.error(
                    actual.span,
                    format!(
                        "actual argument of enumeration type '{}' is not compatible with non-enumeration dummy '{}'; convert with INT(v)",
                        actual_name, dummy.name
                    ),
                );
            }
            _ if !generic_dummy_type_matches(
                ctx,
                interface.declared_scope,
                dummy_type,
                actual_type,
            ) =>
            {
                ctx.error(
                    actual.span,
                    format!(
                        "argument '{}' type mismatch: expected {}, got {}",
                        dummy.name,
                        intrinsic_assignment_type_name(dummy_type),
                        intrinsic_assignment_type_name(actual_type)
                    ),
                );
            }
            _ => {}
        }
    }

    if matches!(dummy.intent, Some(Intent::Out | Intent::InOut))
        && matches!(
            actual_is_definable(ctx, actual, dummy.pointer || dummy.allocatable),
            Some(false)
        )
    {
        ctx.error(
            actual.span,
            format!(
                "actual argument for INTENT(OUT/INOUT) dummy '{}' must be a definable variable",
                dummy.name
            ),
        );
    }

    let Some(actual_rank) = validation_expr_rank(ctx, actual) else {
        return;
    };
    if interface.elemental && dummy.rank == 0 {
        if actual_rank > 0 {
            if let Some(expected_rank) = *elemental_rank {
                if actual_rank != expected_rank {
                    ctx.error(
                        actual.span,
                        format!(
                            "elemental procedure '{}' has nonconforming rank-{expected_rank} and rank-{actual_rank} actual arguments",
                            interface.name
                        ),
                    );
                }
            } else {
                *elemental_rank = Some(actual_rank);
            }
        }
        return;
    }
    if dummy.assumed_rank || actual_rank == dummy.rank {
        return;
    }
    if dummy.allows_sequence_association {
        let character_storage = matches!(
            (&dummy.type_info, validation_expr_type_info(ctx, actual)),
            (
                Some(TypeInfo::Character { .. }),
                Some(TypeInfo::Character { .. })
            )
        );
        if actual_rank > 0 || is_array_element_designator(ctx, actual) || character_storage {
            return;
        }
    }
    ctx.error(
        actual.span,
        format!(
            "argument '{}' rank mismatch: expected rank {}, got rank {}",
            dummy.name, dummy.rank, actual_rank
        ),
    );
}

fn validate_explicit_interface_call(
    ctx: &mut Ctx<'_>,
    callee: &SpannedExpr,
    args: &[Argument],
    expected_kind: DirectProcedureKind,
) {
    let interface = match direct_procedure_interface(ctx, callee, args, expected_kind) {
        ExplicitInterfaceResolution::None => return,
        ExplicitInterfaceResolution::Resolved(interface) => interface,
        ExplicitInterfaceResolution::Invalid(message) => {
            ctx.error(callee.span, message);
            return;
        }
    };
    let mut matched = vec![false; interface.dummies.len()];
    let mut position = 0usize;
    let mut seen_keyword = false;
    let mut reported_too_many = false;
    let mut elemental_rank = None;

    for arg in args {
        let span = argument_span(arg, callee.span);
        let dummy_index = if let Some(keyword) = arg.keyword.as_deref() {
            seen_keyword = true;
            let Some(index) = interface
                .dummies
                .iter()
                .position(|dummy| dummy.name.eq_ignore_ascii_case(keyword))
            else {
                ctx.error(
                    span,
                    format!(
                        "unknown keyword argument '{}' in call to '{}'",
                        keyword, interface.name
                    ),
                );
                continue;
            };
            index
        } else {
            if seen_keyword {
                ctx.error(
                    span,
                    format!(
                        "positional argument follows a keyword argument in call to '{}'",
                        interface.name
                    ),
                );
                continue;
            }
            while position < matched.len() && matched[position] {
                position += 1;
            }
            if position == matched.len() {
                if !reported_too_many {
                    ctx.error(
                        span,
                        format!(
                            "too many actual arguments in call to '{}' (expected at most {})",
                            interface.name,
                            interface.dummies.len()
                        ),
                    );
                    reported_too_many = true;
                }
                continue;
            }
            let index = position;
            position += 1;
            index
        };

        if matched[dummy_index] {
            ctx.error(
                span,
                format!(
                    "duplicate actual argument for dummy '{}' in call to '{}'",
                    interface.dummies[dummy_index].name, interface.name
                ),
            );
            continue;
        }
        matched[dummy_index] = true;
        if let SectionSubscript::Element(actual) = &arg.value {
            validate_explicit_actual(
                ctx,
                &interface,
                &interface.dummies[dummy_index],
                actual,
                &mut elemental_rank,
            );
        }
    }

    for (supplied, dummy) in matched.iter().zip(&interface.dummies) {
        if !supplied && !dummy.optional {
            ctx.error(
                callee.span,
                format!(
                    "missing required argument '{}' in call to '{}'",
                    dummy.name, interface.name
                ),
            );
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

#[derive(Default)]
struct ControlTransferRegionFacts {
    next_region: u64,
    definitions: HashMap<u64, Vec<Vec<u64>>>,
    references: Vec<(u64, Span, Vec<u64>)>,
}

impl ControlTransferRegionFacts {
    fn define(&mut self, label: u64, regions: &[u64]) {
        self.definitions
            .entry(label)
            .or_default()
            .push(regions.to_vec());
    }

    fn reference(&mut self, label: u64, span: Span, regions: &[u64]) {
        self.references.push((label, span, regions.to_vec()));
    }
}

fn collect_control_transfer_region(
    body: &[SpannedStmt],
    regions: &mut Vec<u64>,
    facts: &mut ControlTransferRegionFacts,
) {
    let region = facts.next_region;
    facts.next_region += 1;
    regions.push(region);
    collect_control_transfer_regions(body, regions, facts);
    regions.pop();
}

fn collect_control_transfer_regions(
    stmts: &[SpannedStmt],
    regions: &mut Vec<u64>,
    facts: &mut ControlTransferRegionFacts,
) {
    for stmt in stmts {
        visit_io_branch_labels(&stmt.node, |label| {
            facts.reference(label, stmt.span, regions);
        });
        match &stmt.node {
            Stmt::Goto { label } => facts.reference(*label, stmt.span, regions),
            Stmt::ComputedGoto { labels, .. } => {
                for label in labels {
                    facts.reference(*label, stmt.span, regions);
                }
            }
            Stmt::ArithmeticIf { neg, zero, pos, .. } => {
                for label in [neg, zero, pos] {
                    facts.reference(*label, stmt.span, regions);
                }
            }
            Stmt::Continue { label: Some(label) } => facts.define(*label, regions),
            Stmt::Labeled { label, stmt } => {
                facts.define(*label, regions);
                collect_control_transfer_regions(
                    std::slice::from_ref(stmt.as_ref()),
                    regions,
                    facts,
                );
            }
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                collect_control_transfer_region(then_body, regions, facts);
                for (_, body) in else_ifs {
                    collect_control_transfer_region(body, regions, facts);
                }
                if let Some(body) = else_body {
                    collect_control_transfer_region(body, regions, facts);
                }
            }
            Stmt::IfStmt { action, .. }
            | Stmt::WhereStmt { stmt: action, .. }
            | Stmt::ForallStmt { stmt: action, .. } => {
                collect_control_transfer_region(
                    std::slice::from_ref(action.as_ref()),
                    regions,
                    facts,
                );
            }
            Stmt::DoLoop { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::DoConcurrent { body, .. }
            | Stmt::ForallConstruct { body, .. }
            | Stmt::Block { body, .. }
            | Stmt::Associate { body, .. } => {
                collect_control_transfer_region(body, regions, facts);
            }
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    collect_control_transfer_region(&case.body, regions, facts);
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        TypeGuard::TypeIs { body, .. }
                        | TypeGuard::ClassIs { body, .. }
                        | TypeGuard::ClassDefault { body } => body,
                    };
                    collect_control_transfer_region(body, regions, facts);
                }
            }
            Stmt::SelectRank { guards, .. } => {
                for guard in guards {
                    let body = match guard {
                        RankGuard::Rank { body, .. }
                        | RankGuard::RankStar { body }
                        | RankGuard::RankDefault { body } => body,
                    };
                    collect_control_transfer_region(body, regions, facts);
                }
            }
            Stmt::WhereConstruct {
                body, elsewhere, ..
            } => {
                collect_control_transfer_region(body, regions, facts);
                for (_, body) in elsewhere {
                    collect_control_transfer_region(body, regions, facts);
                }
            }
            _ => {}
        }
    }
}

/// A branch may remain within its current structured region or leave one or
/// more enclosing regions. It may not enter a deeper region or a sibling arm.
fn validate_control_transfer_regions(ctx: &mut Ctx<'_>, body: &[SpannedStmt]) {
    let mut facts = ControlTransferRegionFacts::default();
    collect_control_transfer_regions(body, &mut Vec::new(), &mut facts);

    let mut reported = HashSet::new();
    for (label, span, source_regions) in facts.references {
        let Some(definitions) = facts.definitions.get(&label) else {
            continue;
        };
        // Duplicate and undefined labels have their own diagnostics. Avoid
        // deriving a region result from an ambiguous target.
        let [target_regions] = definitions.as_slice() else {
            continue;
        };
        if source_regions.starts_with(target_regions) {
            continue;
        }
        let key = (label, span.file_id, span.start.line, span.start.col);
        if reported.insert(key) {
            ctx.error(
                span,
                format!("control transfer to label {label} enters a structured construct"),
            );
        }
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
    let type_frame = assocs
        .iter()
        .filter_map(|(name, selector)| {
            validation_expr_type_info(ctx, selector)
                .map(|type_info| (name.to_lowercase(), type_info))
        })
        .collect();
    ctx.associate_frames.push(frame.clone());
    ctx.associate_type_frames.push(type_frame);
    ctx.ambiguity_lexical_frames
        .push(AmbiguityLexicalFrame::Associate { bindings: frame });
    validate_stmts(ctx, body);
    ctx.ambiguity_lexical_frames.pop();
    ctx.associate_type_frames.pop();
    ctx.associate_frames.pop();
}

fn block_binding_frame(
    ctx: &Ctx<'_>,
    block_span: Span,
    implicit: &[SpannedDecl],
    decls: &[SpannedDecl],
) -> HashMap<String, BlockBindingAttrs> {
    let mut frame = HashMap::new();
    let scope_id = ctx
        .st
        .statement_block_scope(block_span)
        .unwrap_or(ctx.scope_id);
    let mut implicit_types = HashMap::new();
    let scope_rules = &ctx.st.scope(scope_id).implicit_rules;
    if !scope_rules.none_type {
        for (letter, implicit_type) in &scope_rules.rules {
            let type_info = match implicit_type {
                ImplicitType::Integer => TypeInfo::Integer { kind: None },
                ImplicitType::Real => TypeInfo::Real { kind: None },
                ImplicitType::DoublePrecision => TypeInfo::DoublePrecision,
                ImplicitType::Complex => TypeInfo::Complex { kind: None },
                ImplicitType::Logical => TypeInfo::Logical { kind: None },
                ImplicitType::Character => TypeInfo::Character {
                    len: Some(1),
                    kind: None,
                },
            };
            implicit_types.insert(*letter, type_info);
        }
    }
    for declaration in implicit {
        match &declaration.node {
            Decl::ImplicitNone { type_, .. } => {
                if *type_ {
                    implicit_types.clear();
                }
            }
            Decl::ImplicitStmt { specs } => {
                for spec in specs {
                    let type_info =
                        crate::sema::resolve::type_resolution::type_spec_to_info_in_scope(
                            &spec.type_spec,
                            ctx.st,
                            scope_id,
                        );
                    for &(start, end) in &spec.ranges {
                        for letter_byte in start as u8..=end as u8 {
                            implicit_types.insert(
                                (letter_byte as char).to_ascii_lowercase(),
                                type_info.clone(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for decl in decls {
        match &decl.node {
            Decl::TypeDecl {
                type_spec,
                attrs,
                entities,
            } => {
                let binding_attrs = block_attrs_from_decl(attrs.as_slice());
                let type_info = Some(
                    crate::sema::resolve::type_resolution::type_spec_to_info_in_scope(
                        type_spec, ctx.st, scope_id,
                    ),
                );
                let dimension_rank = attrs.iter().find_map(|attr| match attr {
                    Attribute::Dimension(dims) => Some(dims.len()),
                    _ => None,
                });
                for entity in entities {
                    let rank = entity
                        .array_spec
                        .as_ref()
                        .map(Vec::len)
                        .or(dimension_rank)
                        .unwrap_or(0);
                    frame.insert(
                        entity.name.to_lowercase(),
                        BlockBindingAttrs {
                            type_info: type_info.clone(),
                            rank: Some(rank),
                            ..binding_attrs.clone()
                        },
                    );
                }
            }
            Decl::ParameterStmt { pairs } => {
                for (name, _) in pairs {
                    frame.insert(
                        name.to_lowercase(),
                        BlockBindingAttrs {
                            parameter: true,
                            rank: Some(0),
                            ..BlockBindingAttrs::default()
                        },
                    );
                }
            }
            Decl::DimensionStmt { entities } => {
                for entity in entities {
                    let type_info =
                        entity.name.chars().next().and_then(|first| {
                            implicit_types.get(&first.to_ascii_lowercase()).cloned()
                        });
                    frame.insert(
                        entity.name.to_lowercase(),
                        BlockBindingAttrs {
                            type_info,
                            rank: Some(entity.array_spec.len()),
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

fn validate_block_dimension_implicit_types(
    ctx: &mut Ctx<'_>,
    block_span: Span,
    implicit: &[SpannedDecl],
    decls: &[SpannedDecl],
) {
    let mut typed_letters = ctx
        .st
        .statement_block_scope(block_span)
        .map(|scope_id| &ctx.st.scope(scope_id).implicit_rules)
        .filter(|rules| !rules.none_type)
        .map(|rules| rules.rules.keys().copied().collect::<HashSet<_>>())
        .unwrap_or_default();

    for declaration in implicit {
        match &declaration.node {
            Decl::ImplicitNone { type_, .. } => {
                if *type_ {
                    typed_letters.clear();
                }
            }
            Decl::ImplicitStmt { specs } => {
                for spec in specs {
                    for &(start, end) in &spec.ranges {
                        for letter_byte in start as u8..=end as u8 {
                            typed_letters.insert((letter_byte as char).to_ascii_lowercase());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for declaration in decls {
        let Decl::DimensionStmt { entities } = &declaration.node else {
            continue;
        };
        for entity in entities {
            let has_type = entity
                .name
                .chars()
                .next()
                .is_some_and(|first| typed_letters.contains(&first.to_ascii_lowercase()));
            if !has_type {
                ctx.error(
                    declaration.span,
                    format!(
                        "array '{}' in DIMENSION statement has no implicit type",
                        entity.name
                    ),
                );
            }
        }
    }
}

fn block_attrs_from_decl(attrs: &[Attribute]) -> BlockBindingAttrs {
    let mut out = BlockBindingAttrs::default();
    for attr in attrs {
        match attr {
            Attribute::Allocatable => out.allocatable = true,
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

// ---- Implicit typing enforcement ----

/// Check that every variable reference is declared or covered by the
/// current scope's implicit typing map.
fn check_implicit_none(
    ctx: &mut Ctx,
    stmts: &[SpannedStmt],
    decls: &[crate::ast::decl::SpannedDecl],
) {
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
    let scope_rules = &ctx.st.scopes[ctx.scope_id].implicit_rules;
    let outer_implicit_letters: std::collections::HashSet<char> = if scope_rules.none_type {
        std::collections::HashSet::new()
    } else {
        scope_rules.rules.keys().copied().collect()
    };
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
            let reason = if ctx.st.is_implicit_none(ctx.scope_id) {
                "IMPLICIT NONE is active"
            } else {
                "no implicit type is available"
            };
            ctx.error(
                *span,
                format!("variable '{}' used but not declared ({reason})", name),
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
            Decl::DimensionStmt { entities } => {
                for entity in entities {
                    declared.insert(entity.name.to_lowercase());
                }
            }
            Decl::AttributeStmt {
                attr: Attribute::External | Attribute::Intrinsic,
                entities,
            } => {
                declared.extend(entities.iter().map(|entity| entity.to_lowercase()));
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
        let ProgramUnit::InterfaceBlock { name, bodies, .. } = &iface.node else {
            continue;
        };
        if let Some(name) = name.as_ref().filter(|name| !name.is_empty()) {
            declared.insert(name.to_lowercase());
        }
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
    let mut imported = std::collections::HashSet::new();
    collect_block_use_binding_names(st, uses, &mut imported);
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
        | "matmul" | "dot_product" | "scale" | "repeat" | "ieee_copy_sign" | "ieee_unordered"
        | "ieee_rem" => (2, Some(2)),
        // Exactly three.
        "ibits" | "merge" | "merge_bits" | "dshiftl" | "dshiftr" | "unpack" | "spread"
        | "ieee_fma" => (3, Some(3)),
        // Optional-argument ranges (F2023 16.9 per-procedure).
        "atan" | "atand" | "atanpi" | "aint" | "anint" | "nint" | "int" | "real" | "logical"
        | "char" | "ichar" | "achar" | "iachar" | "len" | "len_trim" | "floor" | "ceiling"
        | "maskl" | "maskr" | "shape" | "storage_size" | "associated" | "any" | "all" | "norm2"
        | "f_c_string" | "iall" | "iany" | "iparity" | "parity" => (1, Some(2)),
        "cmplx"
        | "size"
        | "lbound"
        | "ubound"
        | "sum"
        | "product"
        | "maxval"
        | "minval"
        | "count"
        | "selected_real_kind"
        | "ieee_selected_real_kind" => (1, Some(3)),
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
        "random_init" => (2, Some(2)),
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
            | "random_init"
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

#[derive(Clone, Copy)]
enum IntrinsicArgumentType {
    Character,
    Integer,
    Logical,
    Real,
}

impl IntrinsicArgumentType {
    fn name(self) -> &'static str {
        match self {
            Self::Character => "CHARACTER",
            Self::Integer => "INTEGER",
            Self::Logical => "LOGICAL",
            Self::Real => "REAL",
        }
    }

    fn matches(self, info: &TypeInfo) -> bool {
        matches!(
            (self, info),
            (Self::Character, TypeInfo::Character { .. })
                | (Self::Integer, TypeInfo::Integer { .. })
                | (Self::Logical, TypeInfo::Logical { .. })
                | (Self::Real, TypeInfo::Real { .. })
        )
    }
}

fn require_intrinsic_argument_type(
    ctx: &mut Ctx<'_>,
    span: Span,
    intrinsic: &str,
    args: &[Argument],
    position: usize,
    keyword: &str,
    expected: IntrinsicArgumentType,
) {
    let Some(actual) = call_rank_argument_expr(args, position, &[keyword])
        .and_then(|expr| validation_expr_type_info(ctx, expr))
    else {
        return;
    };
    if !expected.matches(&actual) {
        ctx.error(
            span,
            format!(
                "intrinsic '{intrinsic}' argument type mismatch: {} must be {}",
                keyword.to_ascii_uppercase(),
                expected.name()
            ),
        );
    }
}

fn require_intrinsic_scalar_argument(
    ctx: &mut Ctx<'_>,
    span: Span,
    intrinsic: &str,
    args: &[Argument],
    position: usize,
    keyword: &str,
) {
    let Some(actual) = call_rank_argument_expr(args, position, &[keyword]) else {
        return;
    };
    if validation_expr_rank(ctx, actual).is_some_and(|rank| rank > 0) {
        ctx.error(
            span,
            format!(
                "intrinsic '{intrinsic}' argument {} must be scalar",
                keyword.to_ascii_uppercase()
            ),
        );
    }
}

fn validate_intrinsic_argument_associations(
    ctx: &mut Ctx<'_>,
    span: Span,
    intrinsic: &str,
    args: &[Argument],
    formals: &[&str],
    required: usize,
) {
    let mut associated = vec![false; formals.len()];
    let mut positional = 0usize;
    let mut saw_keyword = false;

    for arg in args {
        let arg_span = argument_span(arg, span);
        let index = if let Some(keyword) = arg.keyword.as_deref() {
            saw_keyword = true;
            let Some(index) = formals
                .iter()
                .position(|formal| formal.eq_ignore_ascii_case(keyword))
            else {
                ctx.error(
                    arg_span,
                    format!("unknown keyword argument '{keyword}' in call to '{intrinsic}'"),
                );
                continue;
            };
            index
        } else {
            if saw_keyword {
                ctx.error(
                    arg_span,
                    format!(
                        "positional argument follows a keyword argument in call to '{intrinsic}'"
                    ),
                );
                continue;
            }
            let index = positional;
            positional += 1;
            if index >= formals.len() {
                continue;
            }
            index
        };

        if associated[index] {
            ctx.error(
                arg_span,
                format!(
                    "argument '{}' is associated more than once in call to '{intrinsic}'",
                    formals[index]
                ),
            );
        } else {
            associated[index] = true;
        }
    }

    for (index, formal) in formals.iter().enumerate().take(required) {
        if !associated[index] {
            ctx.error(
                span,
                format!("required argument '{formal}' is absent in call to '{intrinsic}'"),
            );
        }
    }
}

fn intrinsic_real_kind(info: &TypeInfo) -> Option<u8> {
    match info {
        TypeInfo::Real { kind } => Some(default_real_kind(*kind)),
        TypeInfo::DoublePrecision => Some(8),
        _ => None,
    }
}

fn validate_intrinsic_result_kind(
    ctx: &mut Ctx<'_>,
    span: Span,
    intrinsic: &str,
    args: &[Argument],
    position: usize,
    character_result: bool,
) {
    let Some(kind_expr) = call_rank_argument_expr(args, position, &["kind"]) else {
        return;
    };
    let integer = validation_expr_type_info(ctx, kind_expr)
        .is_some_and(|info| matches!(info, TypeInfo::Integer { .. }));
    let scalar = validation_expr_rank(ctx, kind_expr).is_none_or(|rank| rank == 0);
    if !integer || !scalar {
        return;
    }

    let kind = match eval_const_int_expr_checked(ctx, kind_expr) {
        Ok(Some(kind)) => kind.value,
        Ok(None) => {
            ctx.error(
                kind_expr.span,
                format!(
                    "intrinsic '{intrinsic}' argument KIND must be a scalar INTEGER constant expression"
                ),
            );
            return;
        }
        Err(error) => {
            ctx.error(error.span, error.msg);
            return;
        }
    };

    if character_result {
        if kind != 1 {
            ctx.error(
                span,
                format!(
                    "CHARACTER(kind={kind}) data is not supported: the backend and runtime support only CHARACTER(kind=1)"
                ),
            );
        }
    } else if !matches!(kind, 1 | 2 | 4 | 8 | 16) {
        ctx.error(
            kind_expr.span,
            format!(
                "intrinsic '{intrinsic}' requests unsupported INTEGER result kind {kind}; supported kinds are 1, 2, 4, 8, and 16"
            ),
        );
    }
}

fn validate_character_intrinsic_call(
    ctx: &mut Ctx<'_>,
    span: Span,
    intrinsic: &str,
    args: &[Argument],
) {
    let Some((formals, required)) = crate::sema::types::character_intrinsic_signature(intrinsic)
    else {
        return;
    };
    validate_intrinsic_argument_associations(ctx, span, intrinsic, args, formals, required);

    let require_type = |ctx: &mut Ctx<'_>, position, formal, expected| {
        require_intrinsic_argument_type(ctx, span, intrinsic, args, position, formal, expected);
    };
    let require_scalar = |ctx: &mut Ctx<'_>, position, formal| {
        require_intrinsic_scalar_argument(ctx, span, intrinsic, args, position, formal);
    };

    match intrinsic {
        "len" | "len_trim" => {
            require_type(ctx, 0, "string", IntrinsicArgumentType::Character);
        }
        "ichar" | "iachar" => {
            require_type(ctx, 0, "c", IntrinsicArgumentType::Character);
        }
        "char" | "achar" => {
            require_type(ctx, 0, "i", IntrinsicArgumentType::Integer);
        }
        "index" => {
            require_type(ctx, 0, "string", IntrinsicArgumentType::Character);
            require_type(ctx, 1, "substring", IntrinsicArgumentType::Character);
            require_type(ctx, 2, "back", IntrinsicArgumentType::Logical);
            require_scalar(ctx, 2, "back");
        }
        "scan" | "verify" => {
            require_type(ctx, 0, "string", IntrinsicArgumentType::Character);
            require_type(ctx, 1, "set", IntrinsicArgumentType::Character);
            require_type(ctx, 2, "back", IntrinsicArgumentType::Logical);
            require_scalar(ctx, 2, "back");
        }
        "adjustl" | "adjustr" => {
            require_type(ctx, 0, "string", IntrinsicArgumentType::Character);
        }
        "trim" => {
            require_type(ctx, 0, "string", IntrinsicArgumentType::Character);
            require_scalar(ctx, 0, "string");
        }
        "repeat" => {
            require_type(ctx, 0, "string", IntrinsicArgumentType::Character);
            require_type(ctx, 1, "ncopies", IntrinsicArgumentType::Integer);
            require_scalar(ctx, 0, "string");
            require_scalar(ctx, 1, "ncopies");
        }
        "lge" | "lgt" | "lle" | "llt" => {
            require_type(ctx, 0, "string_a", IntrinsicArgumentType::Character);
            require_type(ctx, 1, "string_b", IntrinsicArgumentType::Character);
        }
        "new_line" => {
            require_type(ctx, 0, "a", IntrinsicArgumentType::Character);
        }
        "f_c_string" => {
            require_type(ctx, 0, "string", IntrinsicArgumentType::Character);
            require_type(ctx, 1, "asis", IntrinsicArgumentType::Logical);
            require_scalar(ctx, 0, "string");
            require_scalar(ctx, 1, "asis");
        }
        _ => unreachable!(),
    }

    if let Some(position) = crate::sema::types::character_integer_result_kind_position(intrinsic) {
        require_type(ctx, position, "kind", IntrinsicArgumentType::Integer);
        require_scalar(ctx, position, "kind");
        validate_intrinsic_result_kind(ctx, span, intrinsic, args, position, false);
    } else if matches!(intrinsic, "char" | "achar") {
        require_type(ctx, 1, "kind", IntrinsicArgumentType::Integer);
        require_scalar(ctx, 1, "kind");
        validate_intrinsic_result_kind(ctx, span, intrinsic, args, 1, true);
    }
}

pub(super) fn resolved_intrinsic_name(ctx: &Ctx<'_>, name: &str) -> Option<String> {
    let key = name.to_ascii_lowercase();
    if let Some(symbol) = ctx.lookup_lexical(&key) {
        if !symbol.attrs.external
            && (symbol.attrs.intrinsic || matches!(symbol.kind, SymbolKind::IntrinsicProc))
        {
            let canonical = symbol.name.to_ascii_lowercase();
            return is_intrinsic_name(&canonical).then_some(canonical);
        }
        return None;
    }
    if !ctx.lookup_lexical_named_interfaces(&key).is_empty() {
        return None;
    }
    is_intrinsic_name(&key).then_some(key)
}

fn check_intrinsic_call_types(ctx: &mut Ctx<'_>, span: Span, name: &str, args: &[Argument]) {
    let Some(key) = resolved_intrinsic_name(ctx, name) else {
        return;
    };

    if key == "ieee_fma" {
        ctx.require_std(span, FortranStandard::F2023, "IEEE_FMA");
    }
    validate_elemental_intrinsic_rank_conformance(ctx, span, &key, args);
    validate_character_intrinsic_call(ctx, span, &key, args);

    match key.as_str() {
        "fraction" | "exponent" => {
            require_intrinsic_argument_type(
                ctx,
                span,
                &key,
                args,
                0,
                "x",
                IntrinsicArgumentType::Real,
            );
        }
        "scale" => {
            require_intrinsic_argument_type(
                ctx,
                span,
                &key,
                args,
                0,
                "x",
                IntrinsicArgumentType::Real,
            );
            require_intrinsic_argument_type(
                ctx,
                span,
                &key,
                args,
                1,
                "i",
                IntrinsicArgumentType::Integer,
            );
        }
        "size" => {
            const FORMALS: [&str; 3] = ["array", "dim", "kind"];
            validate_intrinsic_argument_associations(ctx, span, &key, args, &FORMALS, 1);
            require_intrinsic_argument_type(
                ctx,
                span,
                &key,
                args,
                1,
                "dim",
                IntrinsicArgumentType::Integer,
            );
            require_intrinsic_scalar_argument(ctx, span, &key, args, 1, "dim");
            require_intrinsic_argument_type(
                ctx,
                span,
                &key,
                args,
                2,
                "kind",
                IntrinsicArgumentType::Integer,
            );
            require_intrinsic_scalar_argument(ctx, span, &key, args, 2, "kind");
            validate_intrinsic_result_kind(ctx, span, &key, args, 2, false);
        }
        "ieee_fma" => {
            const FORMALS: [&str; 3] = ["a", "b", "c"];
            validate_intrinsic_argument_associations(ctx, span, &key, args, &FORMALS, 3);
            for (position, formal) in FORMALS.iter().enumerate() {
                require_intrinsic_argument_type(
                    ctx,
                    span,
                    &key,
                    args,
                    position,
                    formal,
                    IntrinsicArgumentType::Real,
                );
            }
            let kinds: Vec<u8> = FORMALS
                .iter()
                .enumerate()
                .filter_map(|(position, formal)| {
                    call_rank_argument_expr(args, position, &[*formal])
                        .and_then(|actual| validation_expr_type_info(ctx, actual))
                        .and_then(|info| intrinsic_real_kind(&info))
                })
                .collect();
            if kinds.len() == FORMALS.len() && kinds.iter().any(|kind| *kind != kinds[0]) {
                ctx.error(
                    span,
                    "IEEE_FMA arguments A, B, and C must have the same type and kind",
                );
            }
        }
        "ieee_rem" => {
            const FORMALS: [&str; 2] = ["x", "y"];
            validate_intrinsic_argument_associations(ctx, span, &key, args, &FORMALS, 2);
            for (position, formal) in FORMALS.iter().enumerate() {
                require_intrinsic_argument_type(
                    ctx,
                    span,
                    &key,
                    args,
                    position,
                    formal,
                    IntrinsicArgumentType::Real,
                );
            }
        }
        "ieee_selected_real_kind" => {
            const FORMALS: [&str; 3] = ["p", "r", "radix"];
            validate_intrinsic_argument_associations(ctx, span, &key, args, &FORMALS, 0);
            for (position, formal) in FORMALS.iter().enumerate() {
                require_intrinsic_argument_type(
                    ctx,
                    span,
                    &key,
                    args,
                    position,
                    formal,
                    IntrinsicArgumentType::Integer,
                );
                require_intrinsic_scalar_argument(ctx, span, &key, args, position, formal);
            }
        }
        _ => {}
    }
}

fn intrinsic_name_is_shadowed(ctx: &Ctx<'_>, name: &str) -> bool {
    ctx.lookup_lexical(name)
        .is_some_and(|symbol| !symbol.attrs.intrinsic)
        || !ctx.lookup_lexical_named_interfaces(name).is_empty()
}

fn validate_elemental_intrinsic_rank_conformance(
    ctx: &mut Ctx<'_>,
    span: Span,
    name: &str,
    args: &[Argument],
) {
    if !crate::sema::types::is_elemental_intrinsic(name) {
        return;
    }

    let mut expected_rank = None;
    for actual in args.iter().filter_map(|arg| match &arg.value {
        SectionSubscript::Element(actual) => Some(actual),
        SectionSubscript::Range { .. } => None,
    }) {
        let Some(rank) = validation_expr_rank(ctx, actual).filter(|rank| *rank > 0) else {
            continue;
        };
        if let Some(expected) = expected_rank {
            if rank != expected {
                ctx.error(
                    span,
                    format!(
                        "elemental intrinsic '{name}' has nonconforming rank-{expected} and rank-{rank} arguments"
                    ),
                );
                return;
            }
        } else {
            expected_rank = Some(rank);
        }
    }
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
    let Some(key) = resolved_intrinsic_name(ctx, name) else {
        return;
    };
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
    if crate::sema::specific_intrinsic::specific_intrinsic(name).is_some() {
        return true;
    }
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
        "ieee_fma" | "ieee_rem" |
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
        "system_clock" | "date_and_time" | "cpu_time" | "random_number" | "random_seed" | "random_init" |
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
    fn private_named_types_are_not_visible_through_bare_use() {
        let errors = errors_from(
            "\
module private_types
  implicit none
  private
  public :: visible_t

  type :: hidden_t
    integer :: value
  end type hidden_t

  type :: visible_t
    integer :: value
  end type visible_t
contains
  subroutine internal_access_is_valid()
    type(hidden_t) :: item
    item%value = 1
  end subroutine internal_access_is_valid
end module private_types

program declaration_consumer
  use private_types
  implicit none
  type(hidden_t) :: item
end program declaration_consumer

program extension_consumer
  use private_types
  implicit none
  type, extends(hidden_t) :: child_t
  end type child_t
end program extension_consumer

program allocation_consumer
  use private_types
  implicit none
  class(*), allocatable :: item
  allocate(hidden_t :: item)
end program allocation_consumer

program select_type_consumer
  use private_types
  implicit none
  class(*), allocatable :: item
  select type (item)
  type is (hidden_t)
  class default
  end select
end program select_type_consumer

program function_result_consumer
  use private_types
  implicit none
contains
  type(hidden_t) function make_item()
  end function make_item
end program function_result_consumer

program external_function_consumer
  use private_types
  implicit none
  type(hidden_t), external :: make_item
end program external_function_consumer

program array_constructor_consumer
  use private_types
  implicit none
  class(*), allocatable :: items(:)
  items = [hidden_t ::]
end program array_constructor_consumer

program public_consumer
  use private_types
  implicit none
  type(visible_t) :: item
  item%value = 2
end program public_consumer

program block_consumers
  implicit none
  block
    use private_types, only: visible_t
    type(visible_t) :: imported_item
    imported_item%value = 3
  end block
  block
    type :: local_t
      integer :: value
    end type local_t
    type(local_t) :: local_item
    local_item%value = 4
  end block
end program block_consumers
",
        );

        let inaccessible = errors
            .iter()
            .filter(|error| {
                error.contains("derived type 'hidden_t' is not accessible in this scope")
            })
            .count();
        assert_eq!(inaccessible, 7, "{errors:?}");
        assert_eq!(errors.len(), 7, "{errors:?}");
    }

    #[test]
    fn select_type_and_rank_validate_every_guard_body() {
        let errors = errors_from(
            "\
module guard_validation
  implicit none

  type :: base_t
  end type base_t

  type, extends(base_t) :: child_t
  end type child_t
contains
  pure subroutine check_type(value)
    class(base_t), intent(in) :: value

    select type (value)
    type is (child_t)
      stop
    class is (base_t)
      stop
    class default
      stop
    end select
  end subroutine check_type

  pure subroutine check_rank(values)
    integer, intent(in) :: values(..)

    select rank (values)
    rank (0)
      stop
    rank (*)
      stop
    rank default
      stop
    end select
  end subroutine check_rank
end module guard_validation
",
        );

        assert_eq!(
            errors
                .iter()
                .filter(|error| error.as_str() == "STOP not allowed in pure procedure")
                .count(),
            6,
            "every SELECT TYPE and SELECT RANK guard body must be validated: {errors:?}"
        );
        assert_eq!(errors.len(), 6, "unexpected diagnostics: {errors:?}");
    }

    #[test]
    fn private_components_are_visible_only_to_their_defining_module() {
        let errors = errors_from(
            "\
module private_component_owner
  implicit none

  abstract interface
    subroutine callback_iface()
    end subroutine callback_iface
  end interface

  type, public :: explicit_box
    integer, private :: explicit_hidden = 0
    integer, public :: explicit_shown = 0
    procedure(callback_iface), pointer, private, nopass :: private_callback
  end type explicit_box

  type, public :: default_box
    private
    integer :: default_hidden = 0
    integer, public :: default_shown = 0
  end type default_box
contains
  subroutine owner_access_is_valid(left, right)
    type(explicit_box), intent(out) :: left
    type(default_box), intent(out) :: right
    left%explicit_hidden = 1
    right%default_hidden = 2
    if (associated(left%private_callback)) stop 1
    left = explicit_box(explicit_hidden=3, explicit_shown=4)
    right = default_box(5, 6)
  end subroutine owner_access_is_valid
end module private_component_owner

module module_default_owner
  implicit none
  private
  public :: public_box
  type :: public_box
    integer :: visible = 0
  end type public_box
end module module_default_owner

program component_consumer
  use private_component_owner, only: renamed_box => explicit_box, default_box
  use module_default_owner, only: public_box
  implicit none
  type(renamed_box) :: left
  type(default_box) :: right
  type(public_box) :: unaffected
  left%explicit_hidden = 7
  right%default_hidden = 8
  if (associated(left%private_callback)) stop 2
  left%explicit_shown = 9
  right%default_shown = 10
  unaffected%visible = 11
  left = renamed_box(explicit_hidden=12)
  right = default_box(13)
  left = renamed_box(explicit_shown=14)
  right = default_box(default_shown=15)
  associate (alias => left)
    alias%explicit_hidden = 16
    associate (nested_alias => alias)
      nested_alias%explicit_hidden = 17
    end associate
  end associate
end program component_consumer

module foreign_extension
  use private_component_owner, only: default_box
  implicit none
  type, extends(default_box) :: child_box
    integer :: child_value = 0
  end type child_box
contains
  subroutine foreign_access_is_invalid(value)
    type(child_box), intent(inout) :: value
    value%default_hidden = 18
  end subroutine foreign_access_is_invalid
end module foreign_extension
",
        );

        let inaccessible = errors
            .iter()
            .filter(|error| error.contains("private component"))
            .count();
        assert_eq!(inaccessible, 8, "{errors:?}");
        assert_eq!(errors.len(), 8, "{errors:?}");
    }

    #[test]
    fn same_named_generic_bypasses_private_structure_constructor_checks() {
        let errors = errors_from(
            "\
module string_owner
  implicit none
  private
  public :: string_type
  type :: string_type
    private
    character(len=:), allocatable :: raw
  end type string_type
  interface string_type
    module procedure new_string
  end interface string_type
contains
  function new_string(text) result(value)
    character(len=*), intent(in) :: text
    type(string_type) :: value
    value%raw = text
  end function new_string
end module string_owner

program consumer
  use string_owner, only: string_type
  implicit none
  type(string_type) :: value
  value = string_type('ok')
end program consumer
",
        );

        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn component_access_specs_require_a_module_type_definition() {
        let errors = errors_from(
            "\
program local_types
  implicit none
  type :: explicit_private_t
    integer, private :: hidden
  end type explicit_private_t
  type :: explicit_public_t
    integer, public :: shown
  end type explicit_public_t
  type :: default_private_t
    private
    integer :: hidden
  end type default_private_t
end program local_types
",
        );

        assert_eq!(
            errors
                .iter()
                .filter(|error| error.contains("module specification part"))
                .count(),
            3,
            "{errors:?}"
        );
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn generic_tkr_compatibility_preserves_type_direction() {
        let st = SymbolTable::new();
        let ctx = Ctx::new(&st, None, false, false);
        let data = |type_info| GenericDataCharacteristics {
            type_info: Some(type_info),
            declared_scope: 0,
            rank: 0,
            assumed_rank: false,
            assumed_size: false,
            allocatable: false,
            pointer: false,
            intent: None,
        };
        let unlimited = data(TypeInfo::ClassStar);
        let integer = data(TypeInfo::Integer { kind: Some(4) });
        let assumed_type = data(TypeInfo::TypeStar);
        let assumed_size_type = GenericDataCharacteristics {
            rank: 1,
            assumed_size: true,
            ..data(TypeInfo::TypeStar)
        };

        assert!(generic_data_tkr_compatible(&ctx, &unlimited, &integer));
        assert!(!generic_data_tkr_compatible(&ctx, &integer, &unlimited));
        assert!(!generic_data_distinguishable(&ctx, &unlimited, &integer));
        assert!(generic_data_distinguishable(&ctx, &assumed_type, &integer));
        assert!(!generic_data_tkr_compatible(
            &ctx,
            &assumed_size_type,
            &assumed_type
        ));
        assert!(!generic_data_distinguishable(
            &ctx,
            &assumed_size_type,
            &assumed_type
        ));
    }

    #[test]
    fn rejects_direct_subroutine_argument_type_kind_and_rank_mismatches() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  subroutine accept(value, values)
    integer(8), intent(in) :: value
    real(4), intent(in) :: values(:)
  end subroutine accept

  subroutine invoke()
    logical(1) :: flag
    integer(4) :: narrow
    real(4) :: scalar, values(2)
    call accept(flag, values)
    call accept(narrow, values)
    call accept(1_8, scalar)
  end subroutine invoke
end module call_contracts
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("argument 'value' type mismatch")
                    && err.contains("expected INTEGER(8), got LOGICAL(1)")
            }),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("argument 'value' type mismatch")
                    && err.contains("expected INTEGER(8), got INTEGER(4)")
            }),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("argument 'values' rank mismatch")
                    && err.contains("expected rank 1, got rank 0")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_direct_function_argument_type_mismatch() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  integer function classify(value) result(code)
    integer(8), intent(in) :: value
    code = int(value)
  end function classify

  subroutine invoke()
    logical(1) :: flag
    integer :: code
    code = classify(flag)
  end subroutine invoke
end module call_contracts
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("argument 'value' type mismatch")
                    && err.contains("expected INTEGER(8), got LOGICAL(1)")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn direct_call_validation_preserves_complex_part_kind_in_current_scope() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  real(8) function earlier(a) result(value)
    real(8), intent(in) :: a
    value = a
  end function earlier

  real(4) function accept_good(value_good) result(value)
    real(4), intent(in) :: value_good
    value = value_good
  end function accept_good

  real(4) function accept_bad(value_bad) result(value)
    real(4), intent(in) :: value_bad
    value = value_bad
  end function accept_bad

  subroutine invoke(a, z)
    complex(4), intent(in) :: a
    complex(8), intent(in) :: z
    real(4) :: value
    value = accept_good(a%re) + accept_good(a%im)
    value = accept_bad(z%re) + accept_bad(z%im)
  end subroutine invoke
end module call_contracts
",
        );

        assert!(
            !errs.iter().any(|err| err.contains("'value_good'")),
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| {
                    err.contains("argument 'value_bad' type mismatch")
                        && err.contains("expected REAL(4), got REAL(8)")
                })
                .count(),
            2,
            "{errs:?}"
        );
        assert_eq!(errs.len(), 2, "{errs:?}");
    }

    #[test]
    fn rejects_direct_call_argument_association_errors() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  subroutine accept(required, extra)
    integer, intent(in) :: required
    integer, intent(in), optional :: extra
  end subroutine accept

  subroutine invoke()
    call accept(extra=1)
    call accept(required=1, 2)
    call accept(1, required=2)
    call accept(unknown=1, required=2)
    call accept(1, 2, 3)
    call accept(1)
  end subroutine invoke
end module call_contracts
",
        );

        assert!(
            errs.iter()
                .any(|err| err.contains("missing required argument 'required'")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("positional argument follows a keyword argument")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("duplicate actual argument for dummy 'required'")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("unknown keyword argument 'unknown'")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("too many actual arguments")),
            "{errs:?}"
        );
    }

    #[test]
    fn direct_call_validation_uses_the_resolved_owner_scope() {
        let errs = errors_from(
            "\
module integer_api
  implicit none
contains
  subroutine accept(value)
    integer(8), intent(in) :: value
  end subroutine accept
end module integer_api

module logical_api
  implicit none
contains
  subroutine accept(value)
    logical(1), intent(in) :: value
  end subroutine accept
end module logical_api

program caller
  use integer_api, only: accept
  implicit none
  logical(1) :: flag
  call accept(flag)
end program caller
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("argument 'value' type mismatch")
                    && err.contains("expected INTEGER(8), got LOGICAL(1)")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn validates_dummy_types_from_an_explicit_interface_body() {
        let errs = errors_from(
            "\
program caller
  implicit none
  interface
    subroutine accept(value)
      integer(8), intent(in) :: value
    end subroutine accept
  end interface
  logical(1) :: flag
  call accept(flag)
end program caller
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("argument 'value' type mismatch")
                    && err.contains("expected INTEGER(8), got LOGICAL(1)")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn leaves_separately_defined_externals_as_implicit_interfaces() {
        let errs = errors_from(
            "\
program caller
  implicit none
  external :: accept
  logical(1) :: flag
  call accept(flag)
end program caller

subroutine accept(value)
  integer(8), intent(in) :: value
end subroutine accept
",
        );

        assert!(
            !errs
                .iter()
                .any(|err| err.contains("argument 'value' type mismatch")),
            "{errs:?}"
        );
    }

    #[test]
    fn validates_calls_through_explicit_procedure_entities() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
  abstract interface
    subroutine action_interface(value)
      integer(8), intent(in) :: value
    end subroutine action_interface
    integer function transform_interface(value)
      integer(8), intent(in) :: value
    end function transform_interface
  end interface
contains
  subroutine invoke(action)
    procedure(action_interface) :: action
    procedure(action_interface), pointer :: action_pointer
    procedure(transform_interface), pointer :: transform_pointer
    logical(1) :: flag
    integer :: result
    call action(flag)
    call action_pointer(flag)
    result = transform_pointer(flag)
  end subroutine invoke
end module call_contracts
",
        );

        assert_eq!(
            errs.iter()
                .filter(|err| {
                    err.contains("argument 'value' type mismatch")
                        && err.contains("expected INTEGER(8), got LOGICAL(1)")
                })
                .count(),
            3,
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_unlimited_polymorphic_assumed_rank_actuals() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  subroutine accept(anything)
    class(*), intent(in), dimension(..) :: anything
  end subroutine accept

  subroutine invoke()
    integer :: matrix(2, 2)
    call accept(matrix)
  end subroutine invoke
end module call_contracts
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn accepts_requested_intrinsic_kinds_and_complex_literal_kinds() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  subroutine accept(i, r, z)
    integer(8), intent(in) :: i
    real(8), intent(in) :: r
    complex(8), intent(in) :: z
  end subroutine accept

  subroutine invoke()
    integer :: values(2)
    call accept(int(1, kind=8), real(1, 8), cmplx(1.0, 2.0, kind=8))
    call accept(1_8, 2.0_8, (1.0_8, 2.0_8))
    call accept(size(values, kind=8), 2.0_8, (1.0_8, 2.0_8))
  end subroutine invoke
end module call_contracts
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn rejects_unsupported_size_result_kind() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: values(2)
  print *, size(values, kind=3)
end program p
",
        );

        assert!(
            errs.iter()
                .any(|err| err
                    .contains("intrinsic 'size' requests unsupported INTEGER result kind 3")),
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_sequence_association_rank_remapping() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  real function outer(values) result(total)
    real, intent(in) :: values(:, :, :)
    total = flatten(values, size(values))
  contains
    real function flatten(storage, count)
      integer, intent(in) :: count
      real, intent(in) :: storage(count)
      flatten = sum(storage)
    end function flatten
  end function outer
end module call_contracts
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn accepts_character_storage_sequence_association() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  integer function count_chars(storage) result(count)
    character(len=1), intent(in) :: storage(*)
    count = 4
  end function count_chars

  subroutine invoke()
    character(len=4) :: text
    integer :: count
    count = count_chars(text)
  end subroutine invoke
end module call_contracts
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn same_name_generic_facet_is_not_validated_as_a_direct_specific() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
  interface pick
    module procedure pick
    module procedure pick_logical
  end interface pick
contains
  integer function pick(value)
    integer, intent(in) :: value
    pick = value
  end function pick

  logical function pick_logical(value)
    logical, intent(in) :: value
    pick_logical = value
  end function pick_logical

  subroutine invoke()
    logical :: selected
    selected = pick(.true.)
  end subroutine invoke
end module call_contracts
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn validates_conditional_nil_against_selected_generic_specifics() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
  interface required_generic
    module procedure required_value
  end interface required_generic
  interface optional_generic
    module procedure optional_value
  end interface optional_generic
contains
  integer function required_value(value)
    integer, intent(in) :: value
    required_value = value
  end function required_value

  integer function optional_value(value)
    integer, intent(in), optional :: value
    optional_value = 0
    if (present(value)) optional_value = value
  end function optional_value

  subroutine invoke()
    integer :: result
    result = required_generic((.true. ? 7 : .nil.))
    result = optional_generic((.true. ? 7 : .nil.))
  end subroutine invoke
end module call_contracts
",
        );

        assert_eq!(
            errs.iter().filter(|err| err.contains("C1525")).count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_nondefinable_out_and_inout_actuals() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  subroutine fill(output, state)
    integer, intent(out) :: output
    integer, intent(inout) :: state
    output = state
  end subroutine fill

  integer function make_value()
    make_value = 1
  end function make_value

  subroutine invoke()
    integer, parameter :: constant = 1
    integer :: value, values(2), indices(1)
    indices = [1]
    call fill(1, value)
    call fill(value + 1, value)
    call fill(constant, value)
    call fill(make_value(), value)
    call fill(value, values(indices))
    call fill(value, (value))
    call fill(values(1), values(2))
  end subroutine invoke
end module call_contracts
",
        );

        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("must be a definable variable"))
                .count(),
            6,
            "{errs:?}"
        );
    }

    #[test]
    fn validates_conditional_out_actual_arms_as_associations() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
contains
  subroutine fill(output)
    integer, intent(out) :: output
    output = 1
  end subroutine fill

  subroutine invoke(select_left)
    logical, intent(in) :: select_left
    integer :: left, right
    call fill((select_left ? left : right))
    call fill((select_left ? left : right + 1))
  end subroutine invoke
end module call_contracts
",
        );

        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("must be a definable variable"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_pointer_mediated_definable_actuals() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
  integer, target :: storage
  type :: inner_t
    integer :: value
  end type inner_t
  type :: outer_t
    type(inner_t), pointer :: inner
  end type outer_t
contains
  subroutine set_value(value)
    integer, intent(out) :: value
    value = 1
  end subroutine set_value

  subroutine update_target(object)
    type(outer_t), intent(in) :: object
    call set_value(object%inner%value)
  end subroutine update_target

  subroutine update_pointer_target(value)
    integer, pointer, intent(in) :: value
    call set_value(value)
  end subroutine update_pointer_target

  function get_storage() result(pointer_result)
    integer, pointer :: pointer_result
    pointer_result => storage
  end function get_storage

  subroutine invoke()
    call set_value(get_storage())
  end subroutine invoke
end module call_contracts
",
        );

        assert!(
            !errs
                .iter()
                .any(|err| err.contains("must be a definable variable")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_intent_in_pointers_for_pointer_association_dummies() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
  type :: holder_t
    integer, pointer :: value
  end type holder_t
contains
  subroutine reset_pointer(value)
    integer, pointer, intent(out) :: value
    nullify(value)
  end subroutine reset_pointer

  subroutine invoke(actual, object)
    integer, pointer, intent(in) :: actual
    type(holder_t), intent(in) :: object
    call reset_pointer(actual)
    call reset_pointer(object%value)
  end subroutine invoke
end module call_contracts
",
        );

        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("must be a definable variable"))
                .count(),
            2,
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_intent_in_allocatables_for_allocatable_association_dummies() {
        let errs = errors_from(
            "\
module call_contracts
  implicit none
  type :: holder_t
    integer, allocatable :: values(:)
  end type holder_t
contains
  subroutine reset_allocatable(values)
    integer, allocatable, intent(out) :: values(:)
  end subroutine reset_allocatable

  subroutine invoke(values, object)
    integer, allocatable, intent(in) :: values(:)
    type(holder_t), intent(in) :: object
    call reset_allocatable(values)
    call reset_allocatable(object%values)
  end subroutine invoke
end module call_contracts
",
        );

        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("must be a definable variable"))
                .count(),
            2,
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_incompatible_intrinsic_assignment_types() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: left_t
    integer :: value
  end type
  type :: right_t
    integer :: value
  end type
  integer :: number
  logical :: flag
  character(len=4) :: text
  type(left_t) :: left
  type(right_t) :: right
  number = .true.
  flag = 1
  text = 1
  left = right
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            4,
            "{errs:?}"
        );
    }

    #[test]
    fn procedure_dummy_call_uses_later_interface_result_type() {
        let errs = errors_from(
            "\
module m
  implicit none
contains
  integer function wrapper(f) result(r)
    procedure(iface) :: f
    r = f(0)
  end function wrapper

  integer function iface(x)
    integer, intent(in) :: x
    iface = x
  end function iface
end module m
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn rejects_intrinsic_assignment_rank_mismatches() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: scalar
  integer :: vector(2)
  integer :: matrix(2, 2)
  vector = scalar
  scalar = vector
  matrix = vector
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            2,
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_compatible_intrinsic_assignment_conversions() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: item_t
    integer :: value
  end type
  integer(8) :: number
  real(4) :: measure
  complex(8) :: pair
  logical(1) :: small_flag
  logical(4) :: flag
  character(len=2) :: short_text
  character(len=8) :: long_text
  type(item_t) :: left, right
  number = measure
  measure = number
  pair = measure
  small_flag = flag
  short_text = long_text
  left = right
end program
",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn accepts_defined_assignment_for_incompatible_intrinsic_types() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface assignment(=)
    subroutine assign_flag(lhs, rhs)
      integer, intent(out) :: lhs
      logical, intent(in) :: rhs
    end subroutine
  end interface
  integer :: number
  number = .true.
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("intrinsic assignment")),
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_defined_assignment_from_scalar_intrinsic_result() {
        let errs = errors_from(
            "\
module m
  implicit none
  type :: box_t
    character(len=:), allocatable :: text
  end type
  interface assignment(=)
    module procedure assign_box_text
  end interface
  interface trim
    module procedure trim_box
  end interface
contains
  function trim_box(value) result(output)
    type(box_t), intent(in) :: value
    type(box_t) :: output
    output = trim(\" x \")
    output = repeat(\"x\", 2)
    output = new_line(\"x\")
  end function trim_box

  subroutine assign_box_text(lhs, rhs)
    type(box_t), intent(out) :: lhs
    character(len=*), intent(in) :: rhs
    lhs%text = rhs
  end subroutine assign_box_text
end module m
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn accepts_defined_assignment_from_scalar_inquiry_results() {
        let errs = errors_from(
            "\
module m
  implicit none
  type :: box_t
    integer :: value
  end type
  interface assignment(=)
    module procedure assign_box_integer
  end interface
contains
  subroutine exercise(input, output)
    real, intent(in) :: input
    type(box_t), intent(out) :: output
    output = selected_int_kind(9)
    output = precision(input)
    output = command_argument_count()
  end subroutine exercise

  subroutine assign_box_integer(lhs, rhs)
    type(box_t), intent(out) :: lhs
    integer, intent(in) :: rhs
    lhs%value = rhs
  end subroutine assign_box_integer
end module m
",
        );

        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn accepts_type_bound_defined_assignment_for_mixed_types() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  contains
    procedure :: assign_integer
    generic :: assignment(=) => assign_integer
  end type
  type(box_t) :: box
  box = 7
contains
  subroutine assign_integer(lhs, rhs)
    class(box_t), intent(out) :: lhs
    integer, intent(in) :: rhs
    lhs%value = rhs
  end subroutine
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("intrinsic assignment")),
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_defined_assignment_with_unlimited_polymorphic_rhs() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  end type
  interface assignment(=)
    subroutine assign_any(lhs, rhs)
      import :: box_t
      type(box_t), intent(out) :: lhs
      class(*), intent(in) :: rhs
    end subroutine
  end interface
  type(box_t) :: box
  box = 7
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("intrinsic assignment")),
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_block_local_defined_assignment_interface() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  end type
  type(box_t) :: box
  block
    interface assignment(=)
      procedure :: assign_integer
    end interface
    box = 7
  end block
contains
  subroutine assign_integer(lhs, rhs)
    type(box_t), intent(out) :: lhs
    integer, intent(in) :: rhs
    lhs%value = rhs
  end subroutine
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("intrinsic assignment")),
            "{errs:?}"
        );
    }

    #[test]
    fn block_local_defined_assignment_is_not_visible_after_exit() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  end type
  type(box_t) :: box
  block
    interface assignment(=)
      procedure :: assign_integer
    end interface
  end block
  box = 7
contains
  subroutine assign_integer(lhs, rhs)
    type(box_t), intent(out) :: lhs
    integer, intent(in) :: rhs
    lhs%value = rhs
  end subroutine
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn block_local_defined_assignment_restores_host_interface_after_exit() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  end type
  interface assignment(=)
    procedure :: assign_logical
  end interface
  type(box_t) :: box
  block
    interface assignment(=)
      procedure :: assign_integer
    end interface
    box = 7
  end block
  box = .true.
  box = 7
contains
  subroutine assign_integer(lhs, rhs)
    type(box_t), intent(out) :: lhs
    integer, intent(in) :: rhs
    lhs%value = rhs
  end subroutine
  subroutine assign_logical(lhs, rhs)
    type(box_t), intent(out) :: lhs
    logical, intent(in) :: rhs
    lhs%value = merge(1, 0, rhs)
  end subroutine
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn assignment_uses_block_imported_types_and_ranks() {
        let errs = errors_from(
            "\
module imported_values
  implicit none
  logical :: value
  integer :: values(2)
end module
program p
  implicit none
  integer :: value
  integer :: values
  block
    use imported_values, only: value, values
    logical :: flag
    integer :: scalar
    flag = value
    scalar = values
  end block
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("LOGICAL(4) to INTEGER(4)")),
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn block_use_respects_module_nature() {
        let errs = errors_from(
            "\
module iso_fortran_env
  integer, parameter :: shadow_value = 7
end module
program p
  block
    use, intrinsic :: iso_fortran_env, only: int8
    integer(int8) :: intrinsic_value
    intrinsic_value = 1
  end block
  block
    use, non_intrinsic :: iso_fortran_env, only: shadow_value
    integer :: source_value
    source_value = shadow_value
  end block
  block
    use iso_fortran_env, only: shadow_value
    integer :: normal_value
    normal_value = shadow_value
  end block
end program
",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn rejects_incompatible_block_and_component_assignments() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: holder_t
    integer :: values(2)
  end type
  type(holder_t) :: holder
  block
    integer :: local
    local = .true.
  end block
  holder%values(1) = .true.
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err
                    .contains("intrinsic assignment cannot convert LOGICAL(4) to INTEGER(4)"))
                .count(),
            2,
            "{errs:?}"
        );
    }

    #[test]
    fn scalar_character_component_substring_has_scalar_rank() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: holder_t
    character(8) :: text
  end type
  type(holder_t) :: holder
  character :: first
  first = holder%text(1:1)
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("intrinsic assignment rank mismatch")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_binary_expression_rank_mismatch() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: scalar
  integer :: vector(2)
  scalar = vector + 1
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn validates_c_pointer_assignment_types() {
        let errs = errors_from(
            "\
program p
  use iso_c_binding, only: c_ptr, c_funptr, c_null_ptr, c_null_funptr, c_funloc
  implicit none
  type(c_ptr) :: data_pointer
  type(c_funptr) :: procedure_pointer
  data_pointer = c_null_ptr
  procedure_pointer = c_null_funptr
  procedure_pointer = c_funloc(callback)
  data_pointer = procedure_pointer
  procedure_pointer = data_pointer
  data_pointer = .true.
contains
  subroutine callback() bind(c)
  end subroutine
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            3,
            "{errs:?}"
        );
    }

    #[test]
    fn procedure_component_call_uses_interface_result_type_and_rank() {
        let errs = errors_from(
            "\
program p
  implicit none
  abstract interface
    integer function get_value()
    end function
    function get_values() result(values)
      integer :: values(2)
    end function
  end interface
  type :: holder_t
    procedure(get_value), pointer, nopass :: get
    procedure(get_values), pointer, nopass :: get_many
  end type
  type(holder_t) :: holder
  integer :: value
  logical :: flag
  value = holder%get()
  flag = holder%get()
  value = holder%get_many()
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn generic_function_call_uses_selected_result_type_and_rank() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface pick
    function make_values(value) result(values)
      integer, intent(in) :: value
      logical :: values(2)
    end function
  end interface
  integer :: scalar
  scalar = pick(1)
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn type_bound_generic_call_uses_selected_result_type_and_rank() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: picker_t
  contains
    procedure :: pick_integer
    procedure :: pick_logical
    generic :: pick => pick_integer, pick_logical
  end type
  type(picker_t) :: picker
  integer :: scalar
  scalar = picker%pick(.true.)
contains
  integer function pick_integer(self, value)
    class(picker_t), intent(in) :: self
    integer, intent(in) :: value
    pick_integer = value
  end function
  function pick_logical(self, value) result(values)
    class(picker_t), intent(in) :: self
    logical, intent(in) :: value
    logical :: values(2)
    values = value
  end function
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_elemental_function_call_rank_mismatches() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: scalar
  integer :: values(2)
  real :: real_scalar
  real :: real_values(2)
  scalar = abs(values)
  scalar = twice(values)
  scalar = cmplx(values)
  real_scalar = gamma(real_values)
  real_scalar = acosd(real_values)
  scalar = ior(values, 1)
  real_scalar = fraction(real_values)
  scalar = exponent(real_values)
  real_scalar = scale(real_values, 2)
contains
  elemental integer function twice(value)
    integer, intent(in) :: value
    twice = value * 2
  end function
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            9,
            "{errs:?}"
        );
    }

    #[test]
    fn validates_long_intrinsic_operator_chain_with_unrelated_defined_operator() {
        let expression = (0..56).map(|_| "1").collect::<Vec<_>>().join("+");
        let source = format!(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  end type
  interface operator(+)
    function add_boxes(lhs, rhs) result(value)
      import :: box_t
      type(box_t), intent(in) :: lhs, rhs
      type(box_t) :: value
    end function
  end interface
  integer :: total
  total = {expression}
end program
"
        );

        let errs = errors_from(&source);
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn unrelated_defined_operator_does_not_hide_rank_mismatch() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  end type
  interface operator(+)
    function add_boxes(lhs, rhs) result(value)
      import :: box_t
      type(box_t), intent(in) :: lhs, rhs
      type(box_t) :: value
    end function
  end interface
  integer :: scalar
  integer :: values(2)
  scalar = values + 1
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn matching_defined_operator_uses_declared_result_rank() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface operator(.reduce.)
    function reduce_values(lhs, rhs) result(value)
      integer, intent(in) :: lhs(:), rhs(:)
      integer :: value
    end function
  end interface
  integer :: scalar
  integer :: values(2)
  scalar = values .reduce. values
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("intrinsic assignment rank mismatch")),
            "{errs:?}"
        );
    }

    #[test]
    fn matching_defined_operator_uses_declared_result_type() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface operator(.combine.)
    function combine_values(lhs, rhs) result(value)
      integer, intent(in) :: lhs, rhs
      real :: value
    end function
  end interface
  integer :: number
  logical :: flag
  flag = number .combine. number
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn matching_defined_unary_operator_uses_declared_result_rank() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface operator(.collapse.)
    function collapse_values(values) result(value)
      integer, intent(in) :: values(:)
      integer :: value
    end function
  end interface
  integer :: values(2)
  integer :: scalar
  scalar = .collapse. values
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("intrinsic assignment rank mismatch")),
            "{errs:?}"
        );
    }

    #[test]
    fn defined_assignment_requires_matching_character_kind() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface assignment(=)
    subroutine assign_wide(lhs, rhs)
      character(kind=4), intent(out) :: lhs
      integer, intent(in) :: rhs
    end subroutine
  end interface
  character(kind=1) :: text
  text = 1
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn typed_array_constructor_uses_local_named_kind() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer, parameter :: k = 8
  type :: box_t
    integer :: value
  end type
  interface assignment(=)
    subroutine assign_values(lhs, rhs)
      import :: box_t
      type(box_t), intent(out) :: lhs
      integer(8), intent(in) :: rhs(:)
    end subroutine
  end interface
  type(box_t) :: box
  box = [integer(kind=k) :: 1, 2]
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("intrinsic assignment")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_nondefault_character_intrinsics_before_lowering() {
        let errs = errors_from(
            "\
program p
  implicit none
  character(kind=4) :: text
  text = char(65, kind=4)
  text = achar(65, 4)
  text = trim(text)
  text = adjustl(text)
  text = adjustr(text)
  text = repeat(text, 2)
  text = new_line(text)
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("CHARACTER(kind=4) data is not supported")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_unsupported_character_data_forms() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer, parameter :: wide = 4
  character(kind=4) :: scalar
  character(kind=wide), allocatable :: values(:)
  character(kind=selected_char_kind('ISO_10646')) :: selected
  character(kind=4), allocatable :: dynamic
  type :: wrapper
    character(kind=4) :: component
  end type
  print *, wide_'A'
  print *, char(65, kind=4)
  allocate(character(kind=4, len=2) :: dynamic)
end program
",
        );
        assert!(
            errs.iter()
                .filter(|err| err.contains("CHARACTER(kind=4) data is not supported"))
                .count()
                >= 7,
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("CHARACTER(kind=-1) data is not supported")),
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_default_character_data_forms() {
        let errs = errors_from(
            "\
program p
  implicit none
  character(kind=1) :: text
  text = char(65, kind=1)
  text = achar(65, 1)
  text = trim(text)
  text = adjustl(text)
  text = adjustr(text)
  text = repeat(text, 2)
  text = new_line(text)
end program
",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn accepts_canonical_character_intrinsic_keyword_association() {
        let errs = errors_from(
            "\
program p
  use iso_fortran_env, only: int8, int16, int64
  implicit none
  integer(int64) :: i
  character(8) :: text
  logical(int8) :: back
  back = .true.
  i = index(kind=int64, substring='na', string='banana')
  i = scan(kind=int16, back=back, set='ab', string='cabca')
  i = verify(kind=int8, set='ab', string='abXba')
  i = len(kind=int64, string='abcd')
  i = len_trim(kind=int16, string='ab  ')
  i = ichar(kind=kind(0_int64), c='A')
  i = iachar(kind=int8, c='B')
  i = index(kind=selected_int_kind(18), substring='na', string='banana')
  i = verify(kind=selected_real_kind(r=300, p=15), set='ab', string='abXba')
  text = repeat(ncopies=3, string='xy')
  text = adjustl(string='  xy    ')
  text = adjustr(string='xy      ')
  text = trim(string='xy      ')
  text = char(kind=1, i=65)
  text = achar(kind=1, i=66)
  text = new_line(a='x')
  if (lge(string_b='A', string_a='B')) continue
end program p
",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn rejects_invalid_character_intrinsic_association_and_types() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: runtime_kind
  print *, index(string='a', substring='a', reverse=.true.)
  print *, scan('abc', string='abc', set='a')
  print *, verify(string='abc', set='a', back=1)
  print *, len(string='a', kind=runtime_kind)
  print *, ichar(c='a', kind=3)
  print *, repeat(string='a', ncopies=.true.)
  print *, trim(string=1)
  print *, new_line(a=1)
  print *, index(string='a', back=.true.)
  print *, index(string='a', 'a')
end program p
",
        );
        for expected in [
            "unknown keyword argument 'reverse' in call to 'index'",
            "argument 'string' is associated more than once in call to 'scan'",
            "BACK must be LOGICAL",
            "KIND must be a scalar INTEGER constant expression",
            "unsupported INTEGER result kind 3",
            "NCOPIES must be INTEGER",
            "STRING must be CHARACTER",
            "A must be CHARACTER",
            "required argument 'substring' is absent in call to 'index'",
            "positional argument follows a keyword argument in call to 'index'",
        ] {
            assert!(
                errs.iter().any(|error| error.contains(expected)),
                "missing {expected:?} in {errs:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_fraction_exponent_and_scale_types() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: i
  real :: x
  x = fraction(i)
  i = exponent(i)
  x = scale(x, x)
end program
",
        );
        for intrinsic in ["fraction", "exponent", "scale"] {
            assert!(
                errs.iter()
                    .any(|err| err.contains(intrinsic) && err.contains("argument type")),
                "missing {intrinsic} diagnostic: {errs:?}"
            );
        }
    }

    #[test]
    fn validates_exported_ieee_function_contracts() {
        let errs = errors_from(
            "\
program p
  use, intrinsic :: ieee_arithmetic, only : ieee_fma, ieee_rem, ieee_selected_real_kind
  implicit none
  integer :: i
  integer :: values(2)
  real :: r4
  real :: vector(2), matrix(2, 2)
  real(kind=8) :: r8
  r4 = ieee_fma(r4, r4)
  r4 = ieee_fma(r4, r8, r4)
  r4 = ieee_fma(i, r4, r4)
  r4 = ieee_rem(r4, i)
  r4 = ieee_rem(x=r4, unknown=r4)
  matrix = ieee_fma(vector, matrix, matrix)
  i = ieee_selected_real_kind()
  i = ieee_selected_real_kind(p=r4)
  i = ieee_selected_real_kind(r=values)
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("intrinsic 'ieee_fma' takes 3 arguments")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("IEEE_FMA") && err.contains("same type and kind")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("ieee_fma") && err.contains("must be REAL")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("ieee_rem") && err.contains("must be REAL")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("unknown keyword argument 'unknown'") && err.contains("ieee_rem")
            }),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("elemental intrinsic 'ieee_fma'") && err.contains("nonconforming")
            }),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("intrinsic 'ieee_selected_real_kind' takes 1 to 3 arguments")
            }),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("ieee_selected_real_kind")
                    && err.contains("P")
                    && err.contains("must be INTEGER")
            }),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|err| {
                err.contains("ieee_selected_real_kind")
                    && err.contains("R")
                    && err.contains("must be scalar")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn user_ieee_fma_shadows_intrinsic_contract() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface
    integer function ieee_fma(value)
      integer, intent(in) :: value
    end function
  end interface
  integer :: result
  result = ieee_fma(1)
end program
",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn ieee_fma_requires_f2023() {
        let errs = errors_with_std(
            "\
program p
  use, intrinsic :: ieee_arithmetic, only : ieee_fma
  implicit none
  real :: value
  value = ieee_fma(1.0, 2.0, 3.0)
end program
",
            FortranStandard::F2018,
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("IEEE_FMA requires --std=F2023")),
            "{errs:?}"
        );
    }

    #[test]
    fn random_init_requires_f2018() {
        let errs = errors_with_std(
            "\
program p
  implicit none
  call random_init(.true., .false.)
end program
",
            FortranStandard::F2008,
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("RANDOM_INIT requires --std=F2018")),
            "{errs:?}"
        );
    }

    #[test]
    fn random_init_requires_two_scalar_logical_arguments() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: wrong_type
  logical :: wrong_rank(2)
  call random_init(wrong_type, wrong_rank)
  call random_init(.true.)
  call random_init(repeatable=.true., unknown=.false.)
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("REPEATABLE") && err.contains("must be LOGICAL")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("IMAGE_DISTINCT") && err.contains("must be scalar")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("intrinsic 'random_init' takes 2 arguments")),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("unknown keyword argument 'unknown'")),
            "{errs:?}"
        );
    }

    #[test]
    fn user_random_init_shadows_the_intrinsic_contract() {
        let errs = errors_with_std(
            "\
program p
  implicit none
  interface
    subroutine random_init(value)
      integer, intent(in) :: value
    end subroutine
  end interface
  call random_init(7)
end program
",
            FortranStandard::F2008,
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn execute_command_line_cmdmsg_requires_a_scalar_character_variable() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: status, wrong_type
  character(len=32) :: messages(2)
  call execute_command_line('', cmdstat=status, cmdmsg=wrong_type)
  call execute_command_line('', cmdstat=status, cmdmsg=messages)
  call execute_command_line('', cmdstat=status, cmdmsg='literal')
end program
",
        );
        assert!(
            errs.iter()
                .filter(|err| err.contains("CMDMSG") && err.contains("scalar CHARACTER variable"))
                .count()
                >= 3,
            "{errs:?}"
        );
    }

    #[test]
    fn user_execute_command_line_shadows_intrinsic_cmdmsg_validation() {
        let errs = errors_from(
            "\
program p
  implicit none
  interface
    subroutine execute_command_line(value, cmdmsg)
      integer, intent(in) :: value
      integer, intent(out) :: cmdmsg
    end subroutine
  end interface
  integer :: output
  call execute_command_line(7, cmdmsg=output)
end program
",
        );
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn rejects_nonconforming_elemental_intrinsic_ranks() {
        let errs = errors_from(
            "\
program p
  implicit none
  real :: x(2), result(2, 2)
  integer :: exponent_value(2, 2)
  result = scale(x, exponent_value)
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("elemental intrinsic 'scale'")
                    && err.contains("nonconforming")),
            "{errs:?}"
        );
    }

    #[test]
    fn block_local_generic_shadows_intrinsic_arity() {
        let errs = errors_from(
            "\
program p
  implicit none
  block
    interface scale
      procedure :: user_scale
    end interface
    integer :: value
    value = scale(1)
  end block
contains
  integer function user_scale(value)
    integer, intent(in) :: value
    user_scale = value
  end function
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("intrinsic 'scale' takes")),
            "{errs:?}"
        );
    }

    #[test]
    fn selected_char_kind_preserves_leading_whitespace() {
        let errs = errors_from(
            "\
program p
  implicit none
  character(kind=selected_char_kind(' ASCII')) :: text
end program
",
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("CHARACTER(kind=-1) data is not supported")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_mixed_type_untyped_array_constructor() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: values(2)
  values = [1, .true.]
end program
",
        );
        assert!(
            errs.iter().any(
                |err| err.contains("array constructor element type mismatch")
                    && err.contains("expected INTEGER(4), got LOGICAL(4)")
            ),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_nested_block_character_expression_kind_mismatches() {
        let errs = errors_from(
            "\
program p
  implicit none
  block
    character(kind=1) :: narrow
    block
      character(kind=4) :: wide
      narrow = trim(wide)
      narrow = adjustl(wide)
      narrow = adjustr(wide)
      narrow = repeat(wide, 2)
      narrow = new_line(wide)
      narrow = wide // wide
    end block
  end block
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            6,
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_positional_reduction_masks_without_dim_rank() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer :: values(2, 2)
  logical :: mask(2, 2)
  integer :: total
  total = sum(values, mask)
  total = product(values, mask)
  total = maxval(values, mask)
  total = minval(values, mask)
end program
",
        );
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("intrinsic assignment rank mismatch")),
            "{errs:?}"
        );
    }

    #[test]
    fn elemental_defined_assignment_requires_array_lhs_for_array_rhs() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: box_t
    integer :: value
  end type
  interface assignment(=)
    elemental subroutine assign_flag(lhs, rhs)
      import :: box_t
      type(box_t), intent(out) :: lhs
      logical, intent(in) :: rhs
    end subroutine
  end interface
  type(box_t) :: scalar, many(2)
  logical :: flag, flags(2)
  many = flag
  scalar = flags
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment rank mismatch"))
                .count(),
            1,
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("intrinsic assignment cannot convert"))
                .count(),
            1,
            "{errs:?}"
        );
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
    fn rejects_move_alloc_type_kind_and_rank_mismatches() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: first_t
    integer :: value = 0
  end type
  type :: second_t
    integer :: value = 0
  end type
  integer, allocatable :: scalar, vector(:)
  integer(kind=8), allocatable :: wide
  real, allocatable :: real_value
  character(len=5), allocatable :: short
  character(len=9), allocatable :: long
  type(first_t), allocatable :: first
  type(second_t), allocatable :: second

  call move_alloc(scalar, real_value)
  call move_alloc(scalar, wide)
  call move_alloc(scalar, vector)
  call move_alloc(short, long)
  call move_alloc(first, second)
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("MOVE_ALLOC") && err.contains("type and kind"))
                .count(),
            4,
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("MOVE_ALLOC") && err.contains("same rank"))
                .count(),
            1,
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_move_alloc_nonallocatable_designators() {
        let errs = errors_from(
            "\
program p
  implicit none
  integer, allocatable :: value, target
  integer, pointer :: pointer_value
  integer :: plain

  call move_alloc(plain, target)
  call move_alloc(pointer_value, target)
  call move_alloc((value), target)
  call move_alloc(value, plain)
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("MOVE_ALLOC") && err.contains("allocatable variable"))
                .count(),
            4,
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_move_alloc_deferred_length_polymorphic_widening_and_same_variable() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: base_t
  end type
  type, extends(base_t) :: child_t
  end type
  character(len=5), allocatable :: fixed
  character(len=:), allocatable :: deferred
  integer, allocatable :: same
  type(child_t), allocatable :: concrete_child
  class(child_t), allocatable :: polymorphic_child
  class(base_t), allocatable :: polymorphic_base
  class(*), allocatable :: anything, anything_else

  call move_alloc(fixed, deferred)
  call move_alloc(same, same)
  call move_alloc(concrete_child, polymorphic_base)
  call move_alloc(polymorphic_child, polymorphic_base)
  call move_alloc(polymorphic_base, anything)
  call move_alloc(anything, anything_else)
  block
    integer, allocatable :: local_from, local_to
    call move_alloc(local_from, local_to)
  end block
end program
",
        );
        assert!(
            !errs.iter().any(|err| err.contains("MOVE_ALLOC")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_move_alloc_polymorphic_source_to_nonpolymorphic_destination() {
        let errs = errors_from(
            "\
program p
  implicit none
  type :: payload_t
  end type
  class(payload_t), allocatable :: source
  type(payload_t), allocatable :: target
  call move_alloc(source, target)
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("MOVE_ALLOC") && err.contains("polymorphic"))
                .count(),
            1,
            "{errs:?}"
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
  integer :: n
  c = red
  call takes_int(c)
  call takes_color(2)
  call takes_color(c)
  n = takes_color_fn(2)
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
  integer function takes_color_fn(x)
    type(color), intent(in) :: x
    takes_color_fn = int(x)
  end function
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
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("dummy argument 'x' has enumeration type 'color'"))
                .count(),
            2,
            "{errs:?}"
        );
        assert_eq!(errs.len(), 3, "{:?}", errs);
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

    #[test]
    fn bind_c_assumed_length_character_dummy_requires_c_descriptor() {
        let errs = errors_from(
            "\
subroutine inspect_text(text) bind(c, name='inspect_text')
  use iso_c_binding
  character(kind=c_char, len=*), intent(in) :: text
end subroutine inspect_text
",
        );
        assert!(
            errs.iter().any(|e| {
                e.contains("BIND(C) assumed-length CHARACTER dummy 'text'")
                    && e.contains("C descriptors are not implemented")
            }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn bind_c_assumed_length_character_interface_requires_c_descriptor() {
        let errs = errors_from(
            "\
program p
  use iso_c_binding
  interface
    function text_len(text) result(n) bind(c, name='text_len')
      import :: c_char, c_int
      character(kind=c_char) :: text*(*)
      integer(c_int) :: n
    end function text_len
  end interface
end program p
",
        );
        assert!(
            errs.iter().any(|e| {
                e.contains("BIND(C) assumed-length CHARACTER dummy 'text'")
                    && e.contains("C descriptors are not implemented")
            }),
            "{:?}",
            errs
        );
    }

    #[test]
    fn c_character_forms_without_descriptors_remain_supported() {
        let errs = errors_from(
            "\
subroutine inspect_byte(byte) bind(c, name='inspect_byte')
  use iso_c_binding
  character(kind=c_char, len=1), intent(in) :: byte
end subroutine inspect_byte

subroutine inspect_buffer(buffer) bind(c, name='inspect_buffer')
  use iso_c_binding
  character(kind=c_char), intent(in) :: buffer(*)
end subroutine inspect_buffer

subroutine inspect_fortran_text(text)
  character(len=*), intent(in) :: text
end subroutine inspect_fortran_text
",
        );
        assert!(
            !errs.iter().any(|e| e.contains("C descriptors")),
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
    fn rejects_duplicate_allocate_and_deallocate_objects() {
        let errs = errors_from(
            "\
program test
  implicit none
  type :: box_t
    integer, allocatable :: value
  end type
  type(box_t) :: boxes(2)
  integer, allocatable :: values(:)
  integer :: index

  allocate(values(2), values(3))
  allocate(boxes(1)%value, boxes(1)%value)
  deallocate(values, values)
  deallocate(boxes(index)%value, boxes(index)%value)
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| { err.contains("same entity as an earlier ALLOCATE object") })
                .count(),
            2,
            "{errs:?}"
        );
        assert_eq!(
            errs.iter()
                .filter(|err| { err.contains("same entity as an earlier DEALLOCATE object") })
                .count(),
            2,
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_distinct_and_side_effect_selected_allocation_objects() {
        let errs = errors_from(
            "\
program test
  implicit none
  type :: box_t
    integer, allocatable :: value
  end type
  type(box_t) :: boxes(2)

  allocate(boxes(1)%value, boxes(2)%value)
  deallocate(boxes(1)%value, boxes(2)%value)
  allocate(boxes(next_index())%value, boxes(next_index())%value)
contains
  integer function next_index()
    integer, save :: index = 0
    index = index + 1
    next_index = index
  end function
end program
",
        );
        assert!(errs.is_empty(), "{errs:?}");
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

    #[test]
    fn procedure_pointer_declaration_rejects_elemental_interface() {
        let cases = [
            (
                "local",
                "\
program test
  implicit none
  abstract interface
    elemental integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  procedure(callback), pointer :: handler
end program test
",
            ),
            (
                "component",
                "\
program test
  implicit none
  abstract interface
    elemental integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  type :: holder
    procedure(callback), pointer, nopass :: handler
  end type holder
end program test
",
            ),
            (
                "dummy",
                "\
program test
  implicit none
  abstract interface
    elemental integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
contains
  subroutine consume(handler)
    procedure(callback), pointer, intent(in) :: handler
  end subroutine consume
end program test
",
            ),
            (
                "function result",
                "\
program test
  implicit none
  abstract interface
    elemental integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
contains
  function make_handler() result(handler)
    procedure(callback), pointer :: handler
    handler => null()
  end function make_handler
end program test
",
            ),
            (
                "use-associated interface",
                "\
module callback_api
  implicit none
  private
  public :: callback
  abstract interface
    elemental integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
end module callback_api

program test
  use callback_api, only: callback
  implicit none
  procedure(callback), pointer :: handler
end program test
",
            ),
        ];

        for (label, source) in cases {
            let errs = errors_from(source);
            assert_eq!(
                errs,
                ["procedure pointer 'handler' may not have an ELEMENTAL interface"],
                "unexpected diagnostic for {label}"
            );
        }
    }

    #[test]
    fn pointer_to_elemental_intrinsic_remains_nonelemental() {
        let accepted = errors_from(
            "\
program test
  implicit none
  abstract interface
    real function callback(value)
      real, intent(in) :: value
    end function callback
  end interface
  intrinsic :: sin
  procedure(callback), pointer :: handler
  real :: result
  handler => sin
  result = handler(0.0)
end program test
",
        );
        assert!(
            accepted.is_empty(),
            "scalar call through compatible intrinsic target should be accepted: {accepted:?}"
        );

        let rejected = errors_from(
            "\
program test
  implicit none
  abstract interface
    real function callback(value)
      real, intent(in) :: value
    end function callback
  end interface
  intrinsic :: sin
  procedure(callback), pointer :: handler
  real :: result(2)
  handler => sin
  result = handler([0.0, 1.0])
end program test
",
        );
        assert_eq!(
            rejected,
            ["argument 'value' rank mismatch: expected rank 0, got rank 1"]
        );
    }

    #[test]
    fn procedure_pointer_assignment_rejects_data_targets() {
        let cases = [
            (
                "scalar data target",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  integer, target :: storage
  handler => storage
end program test
",
                "procedure pointer 'handler' target 'storage' is not a procedure or procedure pointer",
            ),
            (
                "data pointer target",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  integer, pointer :: storage
  handler => storage
end program test
",
                "procedure pointer 'handler' target 'storage' is not a procedure or procedure pointer",
            ),
            (
                "component procedure pointer and data target",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  type :: holder_t
    procedure(callback), pointer, nopass :: handler
  end type holder_t
  type(holder_t) :: holder
  integer, target :: storage
  holder%handler => storage
end program test
",
                "procedure pointer 'handler' target 'storage' is not a procedure or procedure pointer",
            ),
            (
                "data function result",
                "\
program test
  implicit none
  abstract interface
    integer function callback()
    end function callback
  end interface
  procedure(callback), pointer :: handler
  handler => make_data()
contains
  integer function make_data()
    make_data = 7
  end function make_data
end program test
",
                "procedure pointer 'handler' target 'make_data()' is not a procedure-pointer function result",
            ),
            (
                "abstract interface",
                "\
program test
  implicit none
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => callback
end program test
",
                "procedure pointer 'handler' target 'callback' is an abstract interface and cannot be a procedure target",
            ),
        ];

        for (label, source, expected) in cases {
            let errs = errors_from(source);
            assert_eq!(errs, [expected], "unexpected diagnostic for {label}");
        }
    }

    #[test]
    fn procedure_pointer_assignment_requires_compatible_characteristics() {
        let cases = [
            (
                "procedure nature",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => transform
contains
  integer function transform(value)
    integer, intent(in) :: value
    transform = value
  end function transform
end program test
",
                "procedure nature differs",
            ),
            (
                "dummy count",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value, extra)
    integer, intent(in) :: value
    integer, intent(in) :: extra
  end subroutine action
end program test
",
                "dummy argument count differs",
            ),
            (
                "dummy type",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer(8), intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    logical(1), intent(in) :: value
  end subroutine action
end program test
",
                "dummy argument 1 has a different type",
            ),
            (
                "dummy rank",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer, intent(in) :: value(:)
  end subroutine action
end program test
",
                "dummy argument 1 has a different rank or shape",
            ),
            (
                "dummy intent",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer, intent(out) :: value
    value = 1
  end subroutine action
end program test
",
                "dummy argument 1 has a different INTENT",
            ),
            (
                "dummy value attribute",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, value :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer :: value
  end subroutine action
end program test
",
                "dummy argument 1 has different VALUE attributes",
            ),
            (
                "function result type",
                "\
program test
  implicit none
  abstract interface
    integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  procedure(callback), pointer :: handler
  handler => transform
contains
  logical function transform(value)
    integer, intent(in) :: value
    transform = value > 0
  end function transform
end program test
",
                "function result has a different type",
            ),
            (
                "function result rank",
                "\
program test
  implicit none
  abstract interface
    integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  procedure(callback), pointer :: handler
  handler => transform
contains
  function transform(value) result(result)
    integer, intent(in) :: value
    integer :: result(1)
    result = value
  end function transform
end program test
",
                "function result has a different rank or shape",
            ),
            (
                "pure pointer and impure target",
                "\
program test
  implicit none
  abstract interface
    pure subroutine callback(value)
      integer, intent(inout) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine action
end program test
",
                "target is not PURE",
            ),
            (
                "bind mismatch",
                "\
program test
  use iso_c_binding, only: c_int
  implicit none
  abstract interface
    subroutine callback(value) bind(c)
      import :: c_int
      integer(c_int), value :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer(c_int), value :: value
  end subroutine action
end program test
",
                "BIND(C) attributes differ",
            ),
        ];

        for (label, source, reason) in cases {
            let errs = errors_from(source);
            assert_eq!(
                errs.len(),
                1,
                "unexpected diagnostic count for {label}: {errs:?}"
            );
            assert!(
                errs[0].contains("procedure pointer 'handler'")
                    && errs[0].contains("incompatible characteristics")
                    && errs[0].contains(reason),
                "unexpected diagnostic for {label}: {errs:?}"
            );
        }
    }

    #[test]
    fn procedure_pointer_assignment_checks_full_characteristic_set() {
        let cases = [
            (
                "optional",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, optional :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer :: value
  end subroutine action
end program test
",
                "dummy argument 1 has different OPTIONAL attributes",
            ),
            (
                "allocatable",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, allocatable, intent(inout) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer, intent(inout) :: value
  end subroutine action
end program test
",
                "dummy argument 1 has different ALLOCATABLE attributes",
            ),
            (
                "asynchronous",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, asynchronous :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer :: value
  end subroutine action
end program test
",
                "dummy argument 1 has different ASYNCHRONOUS attributes",
            ),
            (
                "contiguous",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, contiguous :: value(:)
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer :: value(:)
  end subroutine action
end program test
",
                "dummy argument 1 has different CONTIGUOUS attributes",
            ),
            (
                "volatile",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, volatile :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer :: value
  end subroutine action
end program test
",
                "dummy argument 1 has different VOLATILE attributes",
            ),
            (
                "pointer",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, pointer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end program test
",
                "dummy argument 1 has different POINTER attributes",
            ),
            (
                "target",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, target, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end program test
",
                "dummy argument 1 has different TARGET attributes",
            ),
            (
                "character length",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      character(4), intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    character(5), intent(in) :: value
  end subroutine action
end program test
",
                "dummy argument 1 has a different type",
            ),
            (
                "explicit shape",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value(3)
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  subroutine action(value)
    integer, intent(in) :: value(4)
  end subroutine action
end program test
",
                "dummy argument 1 has a different rank or shape",
            ),
            (
                "nested procedure dummy",
                "\
program test
  implicit none
  abstract interface
    subroutine integer_action(value)
      integer, intent(in) :: value
    end subroutine integer_action
  end interface
  abstract interface
    subroutine real_action(value)
      real, intent(in) :: value
    end subroutine real_action
  end interface
  abstract interface
    subroutine callback(action)
      import :: integer_action
      procedure(integer_action) :: action
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => apply
contains
  subroutine apply(action)
    procedure(real_action) :: action
  end subroutine apply
end program test
",
                "dummy argument 1 has incompatible procedure characteristics",
            ),
            (
                "allocatable result",
                "\
program test
  implicit none
  abstract interface
    function callback() result(value)
      integer, allocatable :: value
    end function callback
  end interface
  procedure(callback), pointer :: handler
  handler => make_value
contains
  integer function make_value()
    make_value = 1
  end function make_value
end program test
",
                "function result has different ALLOCATABLE attributes",
            ),
            (
                "pointer result",
                "\
program test
  implicit none
  abstract interface
    function callback() result(value)
      integer, pointer :: value
    end function callback
  end interface
  procedure(callback), pointer :: handler
  handler => make_value
contains
  integer function make_value()
    make_value = 1
  end function make_value
end program test
",
                "function result has different POINTER attributes",
            ),
            (
                "specific intrinsic characteristics",
                "\
program test
  implicit none
  abstract interface
    integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  intrinsic :: sin
  procedure(callback), pointer :: handler
  handler => sin
end program test
",
                "dummy argument 1 has a different type",
            ),
        ];

        for (label, source, reason) in cases {
            let errs = errors_from(source);
            assert_eq!(
                errs.len(),
                1,
                "unexpected diagnostic count for {label}: {errs:?}"
            );
            assert!(
                errs[0].contains("incompatible characteristics") && errs[0].contains(reason),
                "unexpected diagnostic for {label}: {errs:?}"
            );
        }

        let elemental = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler
  handler => action
contains
  elemental subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end program test
",
        );
        assert_eq!(
            elemental,
            ["procedure pointer 'handler' target 'action' is a nonintrinsic ELEMENTAL procedure"]
        );

        let restricted_intrinsic = errors_from(
            "\
program test
  implicit none
  abstract interface
    character function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  intrinsic :: char
  procedure(callback), pointer :: handler
  handler => char
end program test
",
        );
        assert_eq!(
            restricted_intrinsic,
            ["procedure pointer 'handler' target 'char' is not an unrestricted specific intrinsic procedure"]
        );
    }

    #[test]
    fn procedure_pointer_assignment_accepts_compatible_targets() {
        let errs = errors_from(
            "\
module procedure_targets_m
  implicit none
  abstract interface
    integer function callback(value)
      integer, intent(in) :: value(:)
    end function callback
    subroutine impure_callback(value)
      integer, intent(inout) :: value
    end subroutine impure_callback
    subroutine shape_callback(count, values)
      integer, intent(in) :: count
      integer, intent(in) :: values(count)
    end subroutine shape_callback
    real function intrinsic_callback(value)
      real, intent(in) :: value
    end function intrinsic_callback
  end interface
  abstract interface
    function callback_factory() result(result)
      import :: callback
      procedure(callback), pointer :: result
    end function callback_factory
  end interface
  intrinsic :: sin
  procedure(callback), pointer :: first
  procedure(callback), pointer :: second
  procedure(callback_factory), pointer :: factory
  procedure(impure_callback), pointer :: impure_handler
  procedure(shape_callback), pointer :: shape_handler
  procedure(intrinsic_callback), pointer :: intrinsic_handler
  type :: holder_t
    procedure(callback), pointer, nopass :: slot
    procedure(callback_factory), pointer, nopass :: factory_slot
  end type holder_t
  type(holder_t) :: holder
contains
  subroutine configure()
    first => transform
    second => first
    holder%slot => transform
    first => holder%slot
    first => make_handler()
    factory => make_handler
    first => factory()
    holder%factory_slot => make_handler
    first => holder%factory_slot()
    first => null()
    impure_handler => pure_action
    shape_handler => shaped_action
    intrinsic_handler => sin
  end subroutine configure

  integer function transform(value)
    integer, intent(in) :: value(:)
    transform = sum(value)
  end function transform

  function make_handler() result(result)
    procedure(callback), pointer :: result
    result => transform
  end function make_handler

  pure subroutine pure_action(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine pure_action

  subroutine shaped_action(size, items)
    integer, intent(in) :: size
    integer, intent(in) :: items(size)
  end subroutine shaped_action
end module procedure_targets_m
",
        );
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn procedure_pointer_initialization_uses_same_characteristic_rules() {
        let data_target = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  integer, target :: storage
  procedure(callback), pointer :: handler => storage
end program test
",
        );
        assert_eq!(
            data_target,
            ["procedure pointer 'handler' target 'storage' is not a procedure or procedure pointer"]
        );

        let incompatible_target = errors_from(
            "\
module test_m
  implicit none
  abstract interface
    integer function callback()
    end function callback
  end interface
  procedure(callback), pointer :: handler => action
contains
  logical function action()
    action = .true.
  end function action
end module test_m
",
        );
        assert_eq!(
            incompatible_target,
            ["procedure pointer 'handler' target 'action' has incompatible characteristics: function result has a different type"]
        );

        let abstract_target = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler => callback
end program test
",
        );
        assert_eq!(
            abstract_target,
            ["procedure pointer 'handler' target 'callback' is an abstract interface and cannot be a procedure target"]
        );

        let component_data_target = errors_from(
            "\
module test_m
  implicit none
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  integer, target :: storage
  type :: holder_t
    procedure(callback), pointer, nopass :: handler => storage
  end type holder_t
end module test_m
",
        );
        assert_eq!(
            component_data_target,
            ["procedure pointer 'handler' target 'storage' is not a procedure or procedure pointer"]
        );

        let internal_target = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler => action
contains
  subroutine action()
  end subroutine action
end program test
",
        );
        assert_eq!(
            internal_target,
            ["procedure pointer 'handler' initializer 'action' must name an external, module, or unrestricted specific intrinsic procedure"]
        );

        let compatible_module_target = errors_from(
            "\
module test_m
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  procedure(callback), pointer :: handler => action
  type :: holder_t
    procedure(callback), pointer, nopass :: slot => action
  end type holder_t
contains
  subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end module test_m
",
        );
        assert!(
            compatible_module_target.is_empty(),
            "unexpected errors: {compatible_module_target:?}"
        );
    }

    #[test]
    fn procedure_dummy_actual_requires_a_callable_with_matching_characteristics() {
        let data_actual = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  integer :: storage
  call apply(storage)
contains
  subroutine apply(action)
    procedure(callback) :: action
  end subroutine apply
end program test
",
        );
        assert_eq!(
            data_actual,
            ["actual argument for procedure dummy 'action' is not a procedure or procedure pointer"]
        );

        let incompatible_actual = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply

  subroutine action(value)
    real, intent(in) :: value
  end subroutine action
end program test
",
        );
        assert_eq!(
            incompatible_actual,
            ["actual procedure 'action' for dummy 'candidate' has incompatible characteristics: dummy argument 1 has a different type"]
        );

        let function_call_actual = errors_from(
            "\
program test
  implicit none
  abstract interface
    integer function callback()
    end function callback
  end interface
  integer :: storage, result
  result = evaluate(storage)
contains
  integer function evaluate(candidate)
    procedure(callback) :: candidate
    evaluate = candidate()
  end function evaluate
end program test
",
        );
        assert_eq!(
            function_call_actual,
            ["actual argument for procedure dummy 'candidate' is not a procedure or procedure pointer"]
        );

        let compatible_actual = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply

  pure subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end program test
",
        );
        assert!(
            compatible_actual.is_empty(),
            "unexpected errors: {compatible_actual:?}"
        );
    }

    #[test]
    fn procedure_dummy_actual_rejects_noncallable_forms() {
        let cases = [
            (
                "data expression",
                "\
program test
  implicit none
  abstract interface
    integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  integer :: storage
  call apply(storage + 1)
contains
  subroutine apply(action)
    procedure(callback) :: action
  end subroutine apply
end program test
",
                "actual argument for procedure dummy 'action' is not a procedure or procedure pointer",
            ),
            (
                "data function result",
                "\
program test
  implicit none
  abstract interface
    integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  call apply(make_value())
contains
  subroutine apply(action)
    procedure(callback) :: action
  end subroutine apply

  integer function make_value()
    make_value = 1
  end function make_value
end program test
",
                "actual argument for procedure dummy 'action' is not a procedure or procedure pointer",
            ),
            (
                "abstract interface",
                "\
program test
  implicit none
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  call apply(callback)
contains
  subroutine apply(action)
    procedure(callback) :: action
  end subroutine apply
end program test
",
                "actual procedure 'callback' for dummy 'action' is an abstract interface and is not callable",
            ),
            (
                "NULL for nonpointer dummy",
                "\
program test
  implicit none
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  call apply(null())
contains
  subroutine apply(action)
    procedure(callback) :: action
  end subroutine apply
end program test
",
                "actual argument for nonpointer procedure dummy 'action' cannot be NULL()",
            ),
            (
                "restricted intrinsic",
                "\
program test
  implicit none
  abstract interface
    character function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  intrinsic :: char
  call apply(char)
contains
  subroutine apply(action)
    procedure(callback) :: action
  end subroutine apply
end program test
",
                "actual procedure 'char' for dummy 'action' is not an unrestricted specific intrinsic procedure",
            ),
        ];

        for (label, source, expected) in cases {
            let errors = errors_from(source);
            assert_eq!(errors, [expected], "{label}: {errors:?}");
        }
    }

    #[test]
    fn procedure_dummy_actual_checks_full_characteristic_set() {
        let cases = [
            (
                "nature",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply
  integer function action(value)
    integer, intent(in) :: value
    action = value
  end function action
end program test
",
                "procedure nature differs",
            ),
            (
                "dummy count",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply
  subroutine action(value, extra)
    integer, intent(in) :: value, extra
  end subroutine action
end program test
",
                "dummy argument count differs",
            ),
            (
                "dummy rank",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value(:)
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply
  subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end program test
",
                "dummy argument 1 has a different rank or shape",
            ),
            (
                "dummy intent",
                "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply
  subroutine action(value)
    integer, intent(out) :: value
    value = 1
  end subroutine action
end program test
",
                "dummy argument 1 has a different INTENT",
            ),
            (
                "PURE",
                "\
program test
  implicit none
  abstract interface
    pure subroutine callback(value)
      integer, intent(inout) :: value
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply
  subroutine action(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine action
end program test
",
                "target is not PURE",
            ),
            (
                "BIND(C)",
                "\
program test
  use iso_c_binding, only: c_int
  implicit none
  abstract interface
    subroutine callback(value) bind(c)
      import :: c_int
      integer(c_int), value :: value
    end subroutine callback
  end interface
  call apply(action)
contains
  subroutine apply(candidate)
    procedure(callback) :: candidate
  end subroutine apply
  subroutine action(value)
    integer(c_int), value :: value
  end subroutine action
end program test
",
                "BIND(C) attributes differ",
            ),
        ];

        for (label, source, reason) in cases {
            let errors = errors_from(source);
            assert_eq!(
                errors,
                [format!(
                    "actual procedure 'action' for dummy 'candidate' has incompatible characteristics: {reason}"
                )],
                "{label}: {errors:?}"
            );
        }
    }

    #[test]
    fn procedure_pointer_dummy_actual_obeys_pointer_association_rules() {
        let missing_intent = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  call install(action)
contains
  subroutine install(candidate)
    procedure(callback), pointer :: candidate
  end subroutine install
  subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end program test
",
        );
        assert_eq!(
            missing_intent,
            ["nonpointer actual procedure 'action' requires procedure pointer dummy 'candidate' to have INTENT(IN)"]
        );

        let compatible = errors_from(
            "\
program test
  implicit none
  abstract interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
  abstract interface
    function callback_factory() result(result)
      import :: callback
      procedure(callback), pointer :: result
    end function callback_factory
  end interface
  procedure(callback), pointer :: handler
  handler => action
  call consume(handler)
  call consume(make_handler())
  call consume(null())
  call borrow(action)
  call forward(action)
contains
  subroutine consume(candidate)
    procedure(callback), pointer :: candidate
  end subroutine consume

  subroutine borrow(candidate)
    procedure(callback), pointer, intent(in) :: candidate
  end subroutine borrow

  subroutine forward(candidate)
    procedure(callback) :: candidate
    call borrow(candidate)
  end subroutine forward

  function make_handler() result(result)
    procedure(callback), pointer :: result
    result => action
  end function make_handler

  subroutine action(value)
    integer, intent(in) :: value
  end subroutine action
end program test
",
        );
        assert!(compatible.is_empty(), "unexpected errors: {compatible:?}");
    }

    #[test]
    fn optional_procedure_dummy_validates_only_present_conditional_arms() {
        let compatible = errors_from(
            "\
program test
  implicit none
  logical :: enabled
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  enabled = .true.
  call maybe((enabled ? action : .nil.))
contains
  subroutine maybe(candidate)
    procedure(callback), optional :: candidate
  end subroutine maybe
  subroutine action()
  end subroutine action
end program test
",
        );
        assert!(compatible.is_empty(), "unexpected errors: {compatible:?}");

        let invalid_present_arm = errors_from(
            "\
program test
  implicit none
  logical :: enabled
  integer :: storage
  abstract interface
    subroutine callback()
    end subroutine callback
  end interface
  enabled = .true.
  call maybe((enabled ? storage : .nil.))
contains
  subroutine maybe(candidate)
    procedure(callback), optional :: candidate
  end subroutine maybe
end program test
",
        );
        assert_eq!(
            invalid_present_arm,
            ["actual argument for procedure dummy 'candidate' is not a procedure or procedure pointer"]
        );
    }

    #[test]
    fn procedure_dummy_actual_accepts_characteristic_exceptions_and_external_unknowns() {
        let errors = errors_from(
            "\
program test
  abstract interface
    subroutine impure_callback(value)
      integer, intent(inout) :: value
    end subroutine impure_callback
    real function intrinsic_callback(value)
      real, intent(in) :: value
    end function intrinsic_callback
  end interface
  intrinsic :: sin
  external :: separately_compiled
  call apply_impure(pure_action)
  call apply_intrinsic(sin)
  call apply_impure(separately_compiled)
contains
  subroutine apply_impure(candidate)
    procedure(impure_callback) :: candidate
  end subroutine apply_impure

  subroutine apply_intrinsic(candidate)
    procedure(intrinsic_callback) :: candidate
  end subroutine apply_intrinsic

  pure subroutine pure_action(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine pure_action
end program test
",
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
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
    fn pure_control_expressions_reject_impure_function_calls() {
        let errs = errors_from(
            "\
module m
contains
  logical function impure_predicate()
    impure_predicate = .true.
  end function

  integer function impure_bound()
    impure_bound = 1
  end function

  pure subroutine exercise()
    integer :: i

    if (impure_predicate()) continue
    do while (impure_predicate())
      exit
    end do
    do i = impure_bound(), 1
    end do
    select case (impure_bound())
    case (1)
    end select
  end subroutine
end module
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("callee is not pure"))
                .count(),
            4,
            "every control expression must enforce PURE call constraints: {errs:?}"
        );
    }

    #[test]
    fn pure_statement_expression_fields_are_checked_exhaustively() {
        fn errors_for_statement(statement: &str) -> Vec<String> {
            errors_from(&format!(
                "\
module m
  implicit none
contains
  logical function impure_predicate()
    impure_predicate = .true.
  end function

  integer function impure_bound()
    impure_bound = 1
  end function

  function impure_mask() result(mask)
    logical :: mask(2)
    mask = .true.
  end function

  pure subroutine take_scalar(value)
    integer, intent(in) :: value
  end subroutine

  pure subroutine take_vector(value)
    integer, intent(in) :: value(:)
  end subroutine

  pure subroutine exercise()
    integer :: i, values(2)
    integer, allocatable :: allocated_values(:)
    character(32) :: buffer
    values = 0
{statement}
  end subroutine
end module
"
            ))
        }

        let cases = [
            (
                "IF/ELSE IF conditions",
                "    if (impure_predicate()) then\n\
                     continue\n\
                   else if (impure_predicate()) then\n\
                     continue\n\
                   end if",
                2,
            ),
            (
                "single-line IF condition",
                "    if (impure_predicate()) continue",
                1,
            ),
            (
                "DO WHILE condition",
                "    do while (impure_predicate())\n\
                     exit\n\
                   end do",
                1,
            ),
            (
                "counted DO header",
                "    do i = impure_bound(), impure_bound(), impure_bound()\n\
                   end do",
                3,
            ),
            (
                "DO CONCURRENT header",
                "    do concurrent (i = impure_bound():impure_bound():impure_bound(), impure_predicate())\n\
                   end do",
                4,
            ),
            (
                "SELECT CASE selector",
                "    select case (impure_bound())\n\
                   case (1)\n\
                   end select",
                1,
            ),
            (
                "WHERE and ELSEWHERE masks",
                "    where (impure_mask())\n\
                     values = 1\n\
                   elsewhere (impure_mask())\n\
                     values = 2\n\
                   end where",
                2,
            ),
            (
                "FORALL header",
                "    forall (i = impure_bound():impure_bound():impure_bound(), impure_predicate())\n\
                     values(i) = i\n\
                   end forall",
                4,
            ),
            (
                "ASSOCIATE selector",
                "    associate (value => impure_bound())\n\
                     if (value < 0) return\n\
                   end associate",
                1,
            ),
            (
                "internal I/O item",
                "    write(buffer, *) impure_bound()",
                1,
            ),
            (
                "CALL argument",
                "    call take_scalar(impure_bound())",
                1,
            ),
            (
                "ALLOCATE bounds and SOURCE",
                "    allocate(allocated_values(impure_bound()), source=[impure_bound()])",
                2,
            ),
            (
                "conditional-expression arms",
                "    i = (impure_predicate() ? impure_bound() : impure_bound())",
                3,
            ),
            (
                "array-constructor implied-DO bounds",
                "    values = [(i, i = impure_bound(), impure_bound(), impure_bound())]",
                3,
            ),
            (
                "array-section triplet in CALL argument",
                "    call take_vector(values(impure_bound():impure_bound():impure_bound()))",
                3,
            ),
        ];

        for (context, statement, expected) in cases {
            let errs = errors_for_statement(statement);
            assert_eq!(
                errs.iter()
                    .filter(|err| err.contains("callee is not pure"))
                    .count(),
                expected,
                "{context} must inspect every nested function call: {errs:?}"
            );
        }
    }

    #[test]
    fn pure_control_expressions_accept_pure_and_intrinsic_calls() {
        let errs = errors_from(
            "\
module m
  implicit none
contains
  pure logical function pure_predicate()
    pure_predicate = .true.
  end function

  pure integer function pure_bound()
    pure_bound = 1
  end function

  pure subroutine take_scalar(value)
    integer, intent(in) :: value
  end subroutine

  pure subroutine exercise()
    integer :: i
    if (pure_predicate()) then
      do i = abs(pure_bound()), pure_bound()
        call take_scalar(max(i, pure_bound()))
      end do
    end if
  end subroutine
end module
",
        );
        assert!(
            errs.is_empty(),
            "PURE and intrinsic callees must remain valid in control expressions: {errs:?}"
        );
    }

    #[test]
    fn pure_calls_require_an_explicit_pure_contract_for_unknown_and_external_procedures() {
        let errs = errors_from(
            "\
pure subroutine exercise()
  integer :: value
  real, external :: external_value
  real, external :: sin
  external :: external_work

  call unknown_work()
  call external_work()
  value = unknown_value()
  value = int(external_value())
  value = int(sin(0.0))
end subroutine exercise
",
        );
        let contract_errors: Vec<_> = errs
            .iter()
            .filter(|err| err.contains("requires an explicit PURE or ELEMENTAL interface"))
            .collect();
        assert_eq!(
            contract_errors.len(),
            5,
            "every unresolved or EXTERNAL call must require a positive purity contract: {errs:?}"
        );
        for name in [
            "unknown_work",
            "external_work",
            "unknown_value",
            "external_value",
            "sin",
        ] {
            assert!(
                contract_errors.iter().any(|err| err.contains(name)),
                "missing PURE contract diagnostic for {name}: {errs:?}"
            );
        }
    }

    #[test]
    fn pure_calls_accept_explicit_pure_interfaces_intrinsics_and_array_subscripts() {
        let errs = errors_from(
            "\
module callbacks
  abstract interface
    pure integer function pure_callback(value)
      integer, intent(in) :: value
    end function pure_callback
  end interface
contains
  pure subroutine exercise(callback, values)
    procedure(pure_callback) :: callback
    integer, intent(inout) :: values(2)
    intrinsic :: abs

    values(1) = callback(values(2))
    values(2) = abs(values(1))
  end subroutine exercise
end module callbacks
",
        );
        assert!(
            errs.is_empty(),
            "positive PURE contracts and data subscripts must remain valid: {errs:?}"
        );
    }

    #[test]
    fn pure_calls_reject_procedure_dummies_with_impure_interfaces() {
        let errs = errors_from(
            "\
module callbacks
  abstract interface
    integer function impure_callback(value)
      integer, intent(in) :: value
    end function impure_callback
  end interface
contains
  pure integer function exercise(callback, value)
    procedure(impure_callback) :: callback
    integer, intent(in) :: value
    exercise = callback(value)
  end function exercise
end module callbacks
",
        );
        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("requires an explicit PURE or ELEMENTAL interface"))
                .count(),
            1,
            "an impure procedure interface is not a positive PURE contract: {errs:?}"
        );
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

    #[test]
    fn arithmetic_if_requires_scalar_integer_or_real_expression() {
        let logical_errors = errors_from(
            "\
program test
  implicit none
  if (.true.) 10, 20, 30
10 continue
20 continue
30 continue
end program
",
        );
        assert!(
            logical_errors
                .iter()
                .any(|error| error
                    .contains("arithmetic IF expression must be a scalar INTEGER or REAL")),
            "expected logical arithmetic IF rejection, got {logical_errors:?}"
        );

        let array_errors = errors_from(
            "\
program test
  implicit none
  integer :: values(2)
  values = [1, 2]
  if (values) 10, 20, 30
10 continue
20 continue
30 continue
end program
",
        );
        assert!(
            array_errors
                .iter()
                .any(|error| error
                    .contains("arithmetic IF expression must be a scalar INTEGER or REAL")),
            "expected array arithmetic IF rejection, got {array_errors:?}"
        );
    }

    #[test]
    fn arithmetic_if_accepts_scalar_integer_and_real_expressions() {
        let errors = errors_from(
            "\
program test
  implicit none
  integer :: i
  real :: x
  i = 0
  x = 0.0
  if (i) 10, 20, 30
10 continue
  if (x) 20, 30, 40
20 continue
30 continue
40 continue
end program
",
        );
        assert!(
            errors.is_empty(),
            "scalar integer and real arithmetic IF should be valid, got {errors:?}"
        );
    }

    #[test]
    fn inquire_iolength_requires_an_isolated_definable_scalar_integer_result() {
        for invalid_result in [
            "character :: result",
            "integer :: result(2)",
            "integer, parameter :: result = 1",
        ] {
            let errors = errors_from(&format!(
                "\
program test
  implicit none
  {invalid_result}
  inquire(iolength=result) 1
end program
"
            ));
            assert!(
                errors.iter().any(|error| error.contains(
                    "INQUIRE(IOLENGTH=) result must be a definable scalar INTEGER variable"
                )),
                "expected invalid IOLENGTH result rejection for '{invalid_result}', got {errors:?}"
            );
        }

        let mixed_errors = errors_from(
            "\
program test
  implicit none
  integer :: result
  inquire(iolength=result, unit=6) 1
end program
",
        );
        assert!(
            mixed_errors.iter().any(|error| error
                .contains("INQUIRE(IOLENGTH=) may not be combined with other specifiers")),
            "expected mixed INQUIRE form rejection, got {mixed_errors:?}"
        );

        let wrong_form_errors = errors_from(
            "\
program test
  implicit none
  integer :: result
  inquire(unit=6) result
end program
",
        );
        assert!(
            wrong_form_errors
                .iter()
                .any(|error| error.contains("INQUIRE output-item-list requires IOLENGTH=")),
            "expected output-list form rejection, got {wrong_form_errors:?}"
        );

        let empty_list_errors = errors_from(
            "\
program test
  implicit none
  integer :: result
  inquire(iolength=result)
end program
",
        );
        assert!(
            empty_list_errors
                .iter()
                .any(|error| error.contains("INQUIRE(IOLENGTH=) requires an output-item-list")),
            "expected empty IOLENGTH list rejection, got {empty_list_errors:?}"
        );

        let valid_errors = errors_from(
            "\
program test
  implicit none
  integer :: result
  inquire(iolength=result) 1
end program
",
        );
        assert!(
            valid_errors.is_empty(),
            "valid IOLENGTH form should remain accepted, got {valid_errors:?}"
        );
    }

    #[test]
    fn goto_cannot_enter_block_construct() {
        let errors = errors_from(
            "\
program test
  implicit none
  go to 10
  block
10  continue
  end block
end program
",
        );
        assert!(
            errors.iter().any(|error| error
                .contains("control transfer to label 10 enters a structured construct")),
            "branching into a BLOCK must be rejected before lowering: {errors:?}"
        );
    }

    #[test]
    fn computed_goto_and_arithmetic_if_cannot_enter_structured_constructs() {
        let computed_errors = errors_from(
            "\
program test
  implicit none
  integer :: selector
  selector = 1
  go to (10), selector
  if (.true.) then
10  continue
  end if
end program
",
        );
        assert!(
            computed_errors.iter().any(|error| error
                .contains("control transfer to label 10 enters a structured construct")),
            "computed GOTO must not enter an IF arm: {computed_errors:?}"
        );

        let arithmetic_errors = errors_from(
            "\
program test
  implicit none
  integer :: selector
  selector = 0
  if (selector) 20, 20, 20
  do
20  exit
  end do
end program
",
        );
        assert!(
            arithmetic_errors.iter().any(|error| error
                .contains("control transfer to label 20 enters a structured construct")),
            "arithmetic IF must not enter a DO construct: {arithmetic_errors:?}"
        );
    }

    #[test]
    fn io_branch_specifier_cannot_enter_block_construct() {
        let errors = errors_from(
            "\
program test
  implicit none
  integer :: value
  read (*, *, err=25) value
  block
25  continue
  end block
end program
",
        );
        assert!(
            errors.iter().any(|error| error
                .contains("control transfer to label 25 enters a structured construct")),
            "an I/O branch specifier must not enter a BLOCK: {errors:?}"
        );
    }

    #[test]
    fn goto_cannot_cross_between_sibling_if_arms() {
        let errors = errors_from(
            "\
program test
  implicit none
  logical :: choose_first
  choose_first = .true.
  if (choose_first) then
    go to 30
  else
30  continue
  end if
end program
",
        );
        assert!(
            errors.iter().any(|error| error
                .contains("control transfer to label 30 enters a structured construct")),
            "a branch between sibling IF arms must be rejected: {errors:?}"
        );
    }

    #[test]
    fn goto_may_stay_within_or_leave_structured_construct() {
        let errors = errors_from(
            "\
program test
  implicit none
  block
    go to 40
40  continue
    go to 50
  end block
50 continue
end program
",
        );
        assert!(
            !errors
                .iter()
                .any(|error| error.contains("enters a structured construct")),
            "a branch may remain in or leave its current construct: {errors:?}"
        );
    }

    #[test]
    fn goto_may_target_labeled_construct_boundary() {
        let errors = errors_from(
            "\
program test
  implicit none
  go to 60
60 block
     integer :: value
     value = 1
   end block
end program
",
        );
        assert!(
            !errors
                .iter()
                .any(|error| error.contains("enters a structured construct")),
            "the label on a construct statement is outside its body region: {errors:?}"
        );
    }

    // Keep direct context-level tests for undefined/duplicate bookkeeping in
    // addition to the parsed-source region tests above.

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
    fn do_concurrent_default_none_requires_locality_for_outer_variables() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, seed, result(2)
  do concurrent (i = 1:2) default(none)
    result(i) = seed + i
  end do
end program
",
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("'result'") && e.contains("DEFAULT(NONE)")),
            "missing DEFAULT(NONE) diagnostic for result: {errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("'seed'") && e.contains("DEFAULT(NONE)")),
            "missing DEFAULT(NONE) diagnostic for seed: {errs:?}"
        );
    }

    #[test]
    fn do_concurrent_default_none_accepts_explicit_and_block_local_variables() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, seed, result(2)
  seed = 10
  result = 0
  do concurrent (i = 1:2) shared(seed, result) default(none)
    block
      integer :: local_value
      local_value = seed + i
      result(i) = local_value
    end block
  end do
end program
",
        );
        assert!(
            !errs.iter().any(|e| e.contains("DEFAULT(NONE)")),
            "explicit locality and BLOCK-local entities should satisfy DEFAULT(NONE): {errs:?}"
        );
    }

    #[test]
    fn do_concurrent_default_none_reaches_outer_references_in_nested_block() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, seed
  do concurrent (i = 1:2) default(none)
    if (i > 0) then
      block
        integer :: local_value
        local_value = seed
      end block
    end if
  end do
end program
",
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("'seed'") && e.contains("DEFAULT(NONE)")),
            "DEFAULT(NONE) must cover references in nested BLOCKs: {errs:?}"
        );
        assert!(
            !errs.iter().any(|e| e.contains("'local_value'")),
            "a BLOCK-local entity must not require an outer locality-spec: {errs:?}"
        );
    }

    #[test]
    fn do_concurrent_default_none_accepts_use_shadowing_in_nested_block() {
        let errs = errors_from(
            "\
module imported_values
  implicit none
  integer, parameter :: value = 7
end module imported_values

program test
  implicit none
  integer :: i, value, result
  value = 100
  result = 0
  do concurrent (i = 1:2) shared(result) default(none)
    block
      use imported_values, only: value
      result = result + value
    end block
  end do
end program test
",
        );
        assert!(
            !errs.iter().any(|e| e.contains("DEFAULT(NONE)")),
            "a BLOCK use-associated entity must shadow an outer local entity: {errs:?}"
        );
    }

    #[test]
    fn do_concurrent_default_none_accepts_interface_shadowing_in_nested_block() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: i, handler, result
  handler = 100
  result = 0
  do concurrent (i = 1:2) shared(result) default(none)
    block
      interface handler
        pure integer function local_handler(value)
          integer, intent(in) :: value
        end function local_handler
      end interface handler
      result = result + handler(i)
    end block
  end do
end program test
",
        );
        assert!(
            !errs.iter().any(|e| e.contains("DEFAULT(NONE)")),
            "a BLOCK interface must shadow an outer local entity: {errs:?}"
        );
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
    fn nil_arm_to_required_function_dummy_rejected() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: result
  result = required((.true. ? 7 : .nil.))
contains
  integer function required(value)
    integer, intent(in) :: value
    required = value
  end function required
end program
",
        );

        assert!(errs.iter().any(|err| err.contains("C1525")), "{errs:?}");
        assert!(
            !errs.iter().any(|err| err.contains("only valid as an arm")),
            "{errs:?}"
        );
    }

    #[test]
    fn nil_arm_to_optional_function_dummy_accepted() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: result
  result = maybe((.true. ? 7 : .nil.))
contains
  integer function maybe(value)
    integer, intent(in), optional :: value
    if (present(value)) then
      maybe = value
    else
      maybe = 0
    end if
  end function maybe
end program
",
        );

        assert!(!errs.iter().any(|err| err.contains("C1525")), "{errs:?}");
        assert!(
            !errs.iter().any(|err| err.contains("only valid as an arm")),
            "{errs:?}"
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
    fn stop_quiet_requires_f2018() {
        let errs = errors_with_std(
            "\
program test
  implicit none
  stop, quiet=.true.
end program
",
            FortranStandard::F2008,
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("QUIET=") && e.contains("F2018")),
            "{errs:?}"
        );
    }

    #[test]
    fn stop_quiet_requires_a_scalar_logical_expression() {
        let errs = errors_from(
            "\
program test
  implicit none
  integer :: wrong_type
  logical :: wrong_rank(2)
  stop, quiet=wrong_type
  error stop, quiet=wrong_rank
end program
",
        );
        assert_eq!(
            errs.iter()
                .filter(|e| e.contains("QUIET=") && e.contains("scalar LOGICAL"))
                .count(),
            2,
            "{errs:?}"
        );
    }

    #[test]
    fn stop_quiet_accepts_a_scalar_logical_expression() {
        let errs = errors_from(
            "\
program test
  implicit none
  logical :: enabled
  enabled = .true.
  error stop 'boom', quiet=(enabled .and. .true.)
end program
",
        );
        assert!(errs.is_empty(), "{errs:?}");
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
    fn rejects_separate_module_procedure_without_ancestor_interface() {
        let errs = errors_from(
            "\
module missing_interface_parent
  implicit none
end module missing_interface_parent

submodule (missing_interface_parent) missing_interface_child
contains
  module subroutine stray()
  end subroutine stray
end submodule missing_interface_child
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("separate module procedure 'stray'")
                    && err.contains("no matching interface")
                    && err.contains("missing_interface_parent")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_generic_specific_with_ancestor_interface() {
        let errs = errors_from(
            "\
module generic_parent
  implicit none
  interface update
    module subroutine update_integer(value)
      integer, intent(out) :: value
    end subroutine update_integer
  end interface update
end module generic_parent

submodule (generic_parent) generic_child
contains
  module procedure update_integer
    value = 1
  end procedure update_integer
end submodule generic_child
",
        );

        assert!(
            !errs.iter().any(|err| err.contains("no matching interface")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_nonmodule_ancestor_interface_for_separate_body() {
        let errs = errors_from(
            "\
module ordinary_interface_parent
  implicit none
  interface
    subroutine ordinary()
    end subroutine ordinary
  end interface
end module ordinary_interface_parent

submodule (ordinary_interface_parent) ordinary_interface_child
contains
  module subroutine ordinary()
  end subroutine ordinary
end submodule ordinary_interface_child
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("separate module procedure 'ordinary'")
                    && err.contains("no matching interface")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_defined_ancestor_procedure_for_separate_body() {
        let errs = errors_from(
            "\
module defined_procedure_parent
  implicit none
contains
  subroutine already_defined()
  end subroutine already_defined
end module defined_procedure_parent

submodule (defined_procedure_parent) defined_procedure_child
contains
  module subroutine already_defined()
  end subroutine already_defined
end submodule defined_procedure_child
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("separate module procedure 'already_defined'")
                    && err.contains("no matching interface")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn accepts_descendant_bodies_declared_in_root_ancestor() {
        let errs = errors_from(
            "\
module nested_parent
  implicit none
  interface
    module subroutine set_explicit(value)
      integer, intent(out) :: value
    end subroutine set_explicit
    module subroutine set_abbreviated(value)
      integer, intent(out) :: value
    end subroutine set_abbreviated
  end interface
end module nested_parent

submodule (nested_parent) middle
end submodule middle

submodule (nested_parent:middle) leaf
contains
  module subroutine set_explicit(value)
    integer, intent(out) :: value
    value = 1
  end subroutine set_explicit

  module procedure set_abbreviated
    value = 2
  end procedure set_abbreviated
end submodule leaf
",
        );

        assert!(
            !errs.iter().any(|err| err.contains("no matching interface")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_separate_procedure_intent_mismatches() {
        let errs = errors_from(
            "\
module intent_parent
  implicit none
  interface
    module subroutine input_to_output(value)
      integer, intent(in) :: value
    end subroutine input_to_output
    module subroutine inout_to_unspecified(value)
      integer, intent(inout) :: value
    end subroutine inout_to_unspecified
    module subroutine unspecified_to_input(value)
      integer :: value
    end subroutine unspecified_to_input
  end interface
end module intent_parent

submodule (intent_parent) intent_child
contains
  module subroutine input_to_output(value)
    integer, intent(out) :: value
  end subroutine input_to_output
  module subroutine inout_to_unspecified(value)
    integer :: value
  end subroutine inout_to_unspecified
  module subroutine unspecified_to_input(value)
    integer, intent(in) :: value
  end subroutine unspecified_to_input
end submodule intent_child
",
        );

        assert_eq!(
            errs.iter()
                .filter(|err| err.contains("INTENT") && err.contains("does not match"))
                .count(),
            3,
            "{errs:?}"
        );
    }

    #[test]
    fn submodule_implicit_none_applies_to_separate_procedure_bodies() {
        let errs = errors_from(
            "\
module implicit_parent
  interface
    module subroutine run()
    end subroutine run
  end interface
end module implicit_parent

submodule (implicit_parent) implicit_child
  implicit none
contains
  module subroutine run()
    typo = 1
  end subroutine run
end submodule implicit_child
",
        );

        assert!(
            errs.iter().any(|err| {
                err.contains("variable 'typo' used but not declared")
                    && err.contains("IMPLICIT NONE is active")
            }),
            "{errs:?}"
        );
    }

    #[test]
    fn separate_procedure_partially_overrides_submodule_implicit_none() {
        let errs = errors_from(
            "\
module implicit_parent
  interface
    module subroutine run()
    end subroutine run
  end interface
end module implicit_parent

submodule (implicit_parent) implicit_child
  implicit none
contains
  module subroutine run()
    implicit integer(t-t)
    typo = 1
    xray = 2
  end subroutine run
end submodule implicit_child
",
        );

        assert!(
            errs.iter()
                .any(|err| err.contains("variable 'xray' used but not declared")),
            "{errs:?}"
        );
        assert!(
            !errs.iter().any(|err| err.contains("variable 'typo'")),
            "{errs:?}"
        );
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
                imports: vec![],
                implicit: vec![],
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
