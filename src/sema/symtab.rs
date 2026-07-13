//! Symbol table infrastructure.
//!
//! Provides scope-based symbol management with Fortran's four association
//! mechanisms: local declaration, USE association, host association, and
//! IMPORT. Handles implicit typing and case-insensitive lookup.

use crate::ast::decl::ArraySpec;
use crate::ast::expr::SpannedExpr;
use crate::lexer::Span;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Sprint 07: borrow when the input is already canonical lowercase,
/// allocate only when at least one ASCII uppercase byte needs folding.
/// Symbol-table keys are stored in canonical lowercase, so most
/// callers (lowering, type-spec resolution) hand us a pre-lowercased
/// string — this skips ~one allocation per `lookup_in` /
/// `find_symbol_any_scope` call on the hot lookup paths.
fn ensure_ascii_lowercase(s: &str) -> Cow<'_, str> {
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(s.to_ascii_lowercase())
    } else {
        Cow::Borrowed(s)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum LookupMode {
    Normal,
    Exported,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CachedSymbolRef {
    scope_id: ScopeId,
    key: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum UseEntityIdentity {
    Location(CachedSymbolRef),
    DerivedType { owner_module: String, name: String },
}

type PersistentLookupCache = HashMap<ScopeId, HashMap<String, Option<CachedSymbolRef>>>;
type UseAmbiguityCache = HashMap<(ScopeId, String, LookupMode, bool), Option<UseAmbiguity>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UseAmbiguity {
    pub origin_scope: ScopeId,
    pub providers: Vec<String>,
}

#[derive(Debug)]
struct UseCandidate {
    identity: UseEntityIdentity,
    provider: String,
    generic_facet: bool,
}

#[derive(Default)]
struct DirectUseBinding {
    generic: bool,
    non_generic: bool,
}

/// Scope identifier — an index into the SymbolTable's scope list.
pub type ScopeId = usize;

pub fn same_name_generic_interface_key(name: &str) -> String {
    format!(
        "__armfortas_same_name_generic${}",
        ensure_ascii_lowercase(name)
    )
}

fn is_named_interface_like_symbol(sym: &Symbol) -> bool {
    sym.kind == SymbolKind::NamedInterface
        || (sym.kind == SymbolKind::DerivedType && !sym.arg_names.is_empty())
}

fn symbol_exports(sym: &Symbol, scope: &Scope) -> bool {
    match sym.attrs.access {
        Access::Public => true,
        Access::Private => false,
        Access::Default => !matches!(scope.default_access, Access::Private),
    }
}

fn merge_symbol_names(into: &mut Vec<String>, additional: &[String]) {
    for name in additional {
        if !into
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            into.push(name.clone());
        }
    }
}

/// F77 §15.4 statement function: a single-line function defined inside
/// the host procedure's declaration prologue, scoped to that procedure
/// only. Stored on the SymbolTable as a side table so lowering can skip
/// the recognized definition statement and inline-substitute call sites.
#[derive(Debug, Clone)]
pub struct StatementFunctionDef {
    /// Dummy parameter names (lowercase), in declaration order.
    pub params: Vec<String>,
    /// Body expression, exactly as written on the RHS of `name(...) = expr`.
    pub body: SpannedExpr,
    /// Declared result type (from the `type :: name` declaration).
    pub result_type: TypeInfo,
}

/// The symbol table — manages all scopes in a compilation.
#[derive(Debug)]
pub struct SymbolTable {
    pub(crate) scopes: Vec<Scope>,
    pub(crate) current: ScopeId,
    normal_lookup_cache: RefCell<PersistentLookupCache>,
    export_lookup_cache: RefCell<PersistentLookupCache>,
    use_ambiguity_cache: RefCell<UseAmbiguityCache>,
    named_interface_presence_cache: RefCell<HashMap<String, bool>>,
    /// (scope_id, lowercase fname) → statement function definition.
    /// Populated by sema's `detect_statement_functions` pass during
    /// `resolve_unit` for Subroutine/Function/Program arms.
    pub statement_functions: HashMap<(ScopeId, String), StatementFunctionDef>,
}

impl SymbolTable {
    pub fn new() -> Self {
        let global = Scope {
            id: 0,
            parent: None,
            kind: ScopeKind::Global,
            symbols: HashMap::new(),
            implicit_rules: ImplicitRules::default_fortran(),
            has_explicit_implicit_stmt: false,
            use_associations: Vec::new(),
            host_association: HostAssociationControl::all(),
            submodule_ancestor: None,
            default_access: Access::Public,
            pending_access: HashMap::new(),
            arg_order: Vec::new(),
        };
        Self {
            scopes: vec![global],
            current: 0,
            normal_lookup_cache: RefCell::new(HashMap::new()),
            export_lookup_cache: RefCell::new(HashMap::new()),
            use_ambiguity_cache: RefCell::new(HashMap::new()),
            named_interface_presence_cache: RefCell::new(HashMap::new()),
            statement_functions: HashMap::new(),
        }
    }

    fn lookup_cache(&self, mode: LookupMode) -> &RefCell<PersistentLookupCache> {
        match mode {
            LookupMode::Normal => &self.normal_lookup_cache,
            LookupMode::Exported => &self.export_lookup_cache,
        }
    }

    fn clear_lookup_caches(&self) {
        self.normal_lookup_cache.borrow_mut().clear();
        self.export_lookup_cache.borrow_mut().clear();
        self.use_ambiguity_cache.borrow_mut().clear();
        self.named_interface_presence_cache.borrow_mut().clear();
    }

    fn cached_lookup<'a>(
        &'a self,
        mode: LookupMode,
        scope_id: ScopeId,
        key: &str,
    ) -> Option<Option<&'a Symbol>> {
        let cache = self.lookup_cache(mode).borrow();
        let cached = cache.get(&scope_id)?.get(key)?;
        Some(cached.as_ref().and_then(|loc| self.symbol_at(loc)))
    }

    fn remember_lookup(
        &self,
        mode: LookupMode,
        scope_id: ScopeId,
        key: &str,
        result: Option<&Symbol>,
    ) {
        let cached = match result {
            Some(sym) => match self.locate_symbol(sym) {
                Some(loc) => Some(loc),
                None => return,
            },
            None => None,
        };
        self.lookup_cache(mode)
            .borrow_mut()
            .entry(scope_id)
            .or_default()
            .insert(key.to_string(), cached);
    }

    fn symbol_at(&self, loc: &CachedSymbolRef) -> Option<&Symbol> {
        self.scopes.get(loc.scope_id)?.symbols.get(&loc.key)
    }

    fn locate_symbol(&self, sym: &Symbol) -> Option<CachedSymbolRef> {
        let scope = self.scopes.get(sym.scope)?;
        let key = ensure_ascii_lowercase(&sym.name);
        if scope
            .symbols
            .get(key.as_ref())
            .is_some_and(|candidate| std::ptr::eq(candidate, sym))
        {
            return Some(CachedSymbolRef {
                scope_id: sym.scope,
                key: key.into_owned(),
            });
        }
        if !is_named_interface_like_symbol(sym) {
            return None;
        }
        let side_key = same_name_generic_interface_key(key.as_ref());
        scope
            .symbols
            .get(&side_key)
            .is_some_and(|candidate| std::ptr::eq(candidate, sym))
            .then_some(CachedSymbolRef {
                scope_id: sym.scope,
                key: side_key,
            })
    }

    /// Lookup a statement function by (scope, name). Caller passes
    /// the scope where the call site appears; we walk up the parent
    /// chain so a statement function defined in the host is visible
    /// to nested constructs (DO/IF/SELECT bodies don't get their own
    /// procedure scope, so this typically resolves at the same scope).
    pub fn lookup_statement_function(
        &self,
        scope_id: ScopeId,
        name: &str,
    ) -> Option<&StatementFunctionDef> {
        let key = name.to_lowercase();
        let mut cur = Some(scope_id);
        while let Some(sid) = cur {
            if let Some(def) = self.statement_functions.get(&(sid, key.clone())) {
                return Some(def);
            }
            // Statement functions are scope-local to the containing
            // procedure (Subroutine/Function/Program). Stop walking
            // when we leave a procedure scope.
            match self.scopes[sid].kind {
                ScopeKind::Subroutine(_) | ScopeKind::Function(_) | ScopeKind::Program(_) => {
                    return None
                }
                _ => cur = self.scopes[sid].parent,
            }
        }
        None
    }
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTable {
    /// Create a new child scope of the current scope.
    pub fn push_scope(&mut self, kind: ScopeKind) -> ScopeId {
        self.clear_lookup_caches();
        let id = self.scopes.len();
        let parent_implicit = self.scopes[self.current].implicit_rules.clone();
        let scope = Scope {
            id,
            parent: Some(self.current),
            kind,
            symbols: HashMap::new(),
            implicit_rules: parent_implicit, // inherit from parent, may be overridden
            has_explicit_implicit_stmt: false,
            use_associations: Vec::new(),
            host_association: HostAssociationControl::all(),
            submodule_ancestor: None,
            default_access: Access::Public,
            pending_access: HashMap::new(),
            arg_order: Vec::new(),
        };
        self.scopes.push(scope);
        self.current = id;
        id
    }

    /// Enter an existing scope by ID without creating a new one.
    /// Returns the previous scope ID for later restoration.
    pub fn enter_scope(&mut self, id: ScopeId) -> ScopeId {
        let saved = self.current;
        self.current = id;
        saved
    }

    /// Return to the parent scope.
    pub fn pop_scope(&mut self) {
        if let Some(parent) = self.scopes[self.current].parent {
            self.current = parent;
        }
    }

    /// Get the current scope ID.
    pub fn current_scope(&self) -> ScopeId {
        self.current
    }

    /// Get a scope by ID.
    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id]
    }

    /// Get a mutable scope by ID.
    pub fn scope_mut(&mut self, id: ScopeId) -> &mut Scope {
        self.clear_lookup_caches();
        &mut self.scopes[id]
    }

    pub(crate) fn set_host_association_control(
        &mut self,
        scope_id: ScopeId,
        control: HostAssociationControl,
    ) {
        self.clear_lookup_caches();
        self.scopes[scope_id].host_association = control;
    }

    pub(crate) fn set_submodule_ancestor(&mut self, scope_id: ScopeId, ancestor: &str) {
        self.scopes[scope_id].submodule_ancestor = Some(ancestor.to_ascii_lowercase());
    }

    fn import_host_scope(&self, scope_id: ScopeId) -> Option<ScopeId> {
        if let Some(host_scope) = self.scopes[scope_id].host_association.host_scope_override {
            return Some(host_scope);
        }
        let parent = self.scopes[scope_id].parent?;
        if self.scopes[parent].kind == ScopeKind::Interface {
            self.scopes[parent].parent
        } else {
            Some(parent)
        }
    }

    fn host_symbol_precedes_cutoff(&self, scope_id: ScopeId, symbol: &Symbol) -> bool {
        let control = &self.scopes[scope_id].host_association;
        let Some(cutoff) = control.host_declaration_cutoff else {
            return true;
        };
        let Some(host_scope) = self.import_host_scope(scope_id) else {
            return true;
        };
        symbol.scope != host_scope
            || symbol.defined_at.file_id != cutoff.file_id
            || (symbol.defined_at.start.line, symbol.defined_at.start.col)
                < (cutoff.start.line, cutoff.start.col)
    }

    fn host_association_allows_symbol(
        &self,
        scope_id: ScopeId,
        key: &str,
        symbol: &Symbol,
    ) -> bool {
        self.scopes[scope_id].host_association.allows(key)
            && self.host_symbol_precedes_cutoff(scope_id, symbol)
    }

    fn host_association_allows_generic_facet(&self, scope_id: ScopeId, key: &str) -> bool {
        if !self.scopes[scope_id].host_association.allows(key) {
            return false;
        }
        let Some(host_scope) = self.import_host_scope(scope_id) else {
            return true;
        };
        let generic_symbol = self.named_interface_facet_symbol_in_scope(host_scope, key);
        generic_symbol.is_none_or(|symbol| {
            self.host_symbol_precedes_cutoff(scope_id, symbol)
                || self.scope_has_use_associated_generic_facet(host_scope, key)
        })
    }

    pub(crate) fn association_allowed_from_scope(
        &self,
        scope_id: ScopeId,
        association: &UseAssociation,
        key: &str,
    ) -> bool {
        !association.is_submodule_access || self.scopes[scope_id].host_association.allows(key)
    }

    fn scope_has_use_associated_generic_facet(&self, scope_id: ScopeId, key: &str) -> bool {
        let scope = &self.scopes[scope_id];
        let mut visited = vec![(scope_id, key.to_string(), LookupMode::Normal)];
        for association in &scope.use_associations {
            if association.local_name != key {
                continue;
            }
            if !self.association_allowed_from_scope(scope_id, association, key) {
                continue;
            }
            if association.from_bare_use
                && association.local_name == association.original_name
                && self.use_name_is_fully_renamed(scope_id, association.source_scope, key)
            {
                continue;
            }
            let source_mode = if association.is_submodule_access {
                LookupMode::Normal
            } else {
                LookupMode::Exported
            };
            if self.scope_has_generic_facet_guarded(
                association.source_scope,
                &association.original_name,
                source_mode,
                &mut visited,
            ) {
                return true;
            }
        }

        let mut seen_use_scopes = Vec::new();
        for association in &scope.use_associations {
            if !association.from_bare_use
                || association.local_name != association.original_name
                || seen_use_scopes.contains(&association.source_scope)
                || self.use_name_is_fully_renamed(scope_id, association.source_scope, key)
                || !self.association_allowed_from_scope(scope_id, association, key)
            {
                continue;
            }
            seen_use_scopes.push(association.source_scope);
            let source_mode = if association.is_submodule_access {
                LookupMode::Normal
            } else {
                LookupMode::Exported
            };
            if self.scope_has_generic_facet_guarded(
                association.source_scope,
                key,
                source_mode,
                &mut visited,
            ) {
                return true;
            }
        }
        false
    }

    pub(crate) fn inaccessible_host_symbol(
        &self,
        scope_id: ScopeId,
        name: &str,
        allow_generic_facet: bool,
    ) -> Option<&Symbol> {
        let key = ensure_ascii_lowercase(name);
        if self.lookup_in(scope_id, key.as_ref()).is_some() {
            return None;
        }
        if allow_generic_facet
            && self.scope_has_generic_facet(scope_id, key.as_ref(), LookupMode::Normal)
        {
            return None;
        }
        let host_scope = self.import_host_scope(scope_id)?;
        let Some(host_symbol) = self.lookup_in(host_scope, key.as_ref()) else {
            return self.inaccessible_host_symbol(host_scope, key.as_ref(), allow_generic_facet);
        };
        (!self.host_association_allows_symbol(scope_id, key.as_ref(), host_symbol))
            .then_some(host_symbol)
    }

    fn conflicts_with_protected_host_entity(&self, scope_id: ScopeId, key: &str) -> bool {
        if !self.scopes[scope_id].host_association.protects(key) {
            return false;
        }
        let Some(host_scope) = self.import_host_scope(scope_id) else {
            return false;
        };
        let direct_conflict = self.lookup_in(host_scope, key).is_some_and(|host_symbol| {
            self.host_association_allows_symbol(scope_id, key, host_symbol)
        });
        direct_conflict
            || (self
                .named_interface_facet_symbol_in_scope(host_scope, key)
                .is_some()
                && self.host_association_allows_generic_facet(scope_id, key))
    }

    pub(crate) fn current_use_name_conflicts_with_import(&self, name: &str) -> bool {
        let key = ensure_ascii_lowercase(name);
        self.conflicts_with_protected_host_entity(self.current, key.as_ref())
    }

    fn direct_use_binding(&self, scope_id: ScopeId, key: &str) -> DirectUseBinding {
        let scope = &self.scopes[scope_id];
        let mut binding = DirectUseBinding::default();
        for association in &scope.use_associations {
            if association.is_submodule_access || association.local_name != key {
                continue;
            }
            if association.from_bare_use
                && association.local_name == association.original_name
                && self.use_name_is_fully_renamed(scope_id, association.source_scope, key)
            {
                continue;
            }
            let has_entity = self.resolve_use_candidate(association).is_some();
            let has_generic = self.scope_has_generic_facet(
                association.source_scope,
                &association.original_name,
                LookupMode::Exported,
            );
            binding.generic |= has_generic;
            binding.non_generic |= has_entity && !has_generic;
        }

        let mut seen_use_scopes = Vec::new();
        for association in &scope.use_associations {
            if association.is_submodule_access
                || !association.from_bare_use
                || association.local_name != association.original_name
                || seen_use_scopes.contains(&association.source_scope)
                || self.use_name_is_fully_renamed(scope_id, association.source_scope, key)
            {
                continue;
            }
            seen_use_scopes.push(association.source_scope);
            let mut visited = Vec::new();
            let mut cache = HashMap::new();
            let has_entity = self
                .lookup_exported_in_guarded(association.source_scope, key, &mut visited, &mut cache)
                .is_some();
            let has_generic =
                self.scope_has_generic_facet(association.source_scope, key, LookupMode::Exported);
            binding.generic |= has_generic;
            binding.non_generic |= has_entity && !has_generic;
        }
        binding
    }

    fn local_symbol_conflicts_with_direct_use(&self, scope_id: ScopeId, symbol: &Symbol) -> bool {
        let key = ensure_ascii_lowercase(&symbol.name);
        let binding = self.direct_use_binding(scope_id, key.as_ref());
        binding.non_generic || (binding.generic && symbol.kind != SymbolKind::NamedInterface)
    }

    pub(crate) fn current_local_use_conflict(&self) -> Option<(String, Span)> {
        let mut local_symbols: Vec<&Symbol> = self.scopes[self.current].symbols.values().collect();
        local_symbols.sort_by_key(|symbol| {
            (
                symbol.defined_at.file_id,
                symbol.defined_at.start.line,
                symbol.defined_at.start.col,
                symbol.name.to_ascii_lowercase(),
            )
        });
        local_symbols.into_iter().find_map(|symbol| {
            self.local_symbol_conflicts_with_direct_use(self.current, symbol)
                .then(|| (symbol.name.clone(), symbol.defined_at))
        })
    }

    /// Define a symbol in the current scope.
    pub fn define(&mut self, symbol: Symbol) -> Result<(), SemaError> {
        self.clear_lookup_caches();
        let key = symbol.name.to_lowercase();
        if self.conflicts_with_protected_host_entity(self.current, &key) {
            return Err(SemaError {
                span: symbol.defined_at,
                msg: format!(
                    "local declaration '{}' conflicts with an explicitly imported host entity",
                    symbol.name
                ),
            });
        }
        if self.local_symbol_conflicts_with_direct_use(self.current, &symbol) {
            return Err(SemaError {
                span: symbol.defined_at,
                msg: format!(
                    "local declaration '{}' conflicts with a USE-associated entity",
                    symbol.name
                ),
            });
        }
        let local_type_owner = if symbol.kind == SymbolKind::DerivedType {
            match &self.scopes[self.current].kind {
                ScopeKind::Module(name) | ScopeKind::Submodule(name) => {
                    Some(name.to_ascii_lowercase())
                }
                _ => None,
            }
        } else {
            None
        };
        let scope = &mut self.scopes[self.current];
        if scope.symbols.contains_key(&key) {
            return Err(SemaError {
                span: symbol.defined_at,
                msg: format!("symbol '{}' already defined in this scope", symbol.name),
            });
        }
        let mut symbol = symbol;
        symbol.scope = self.current;
        if symbol.attrs.type_owner_module.is_none() {
            symbol.attrs.type_owner_module = local_type_owner;
        }
        if let Some(access) = scope.pending_access.get(&key).copied() {
            symbol.attrs.access = access;
        }
        scope.symbols.insert(key, symbol);
        Ok(())
    }

    pub fn define_same_name_generic_interface(&mut self, mut symbol: Symbol) {
        self.clear_lookup_caches();
        let scope_id = self.current_scope();
        let public_key = symbol.name.to_lowercase();
        let side_key = same_name_generic_interface_key(&public_key);
        let scope = &mut self.scopes[scope_id];
        if let Some(access) = scope.pending_access.get(&public_key).copied() {
            symbol.attrs.access = access;
        }
        symbol.scope = scope_id;
        if let Some(existing) = scope.symbols.get_mut(&side_key) {
            merge_symbol_names(&mut existing.arg_names, &symbol.arg_names);
            return;
        }
        scope.symbols.insert(side_key, symbol);
    }

    pub fn named_interface_symbol_in_scope(
        &self,
        scope_id: ScopeId,
        name: &str,
    ) -> Option<&Symbol> {
        let key = ensure_ascii_lowercase(name);
        let scope = &self.scopes[scope_id];
        if let Some(sym) = scope.symbols.get(key.as_ref()) {
            if is_named_interface_like_symbol(sym) {
                return Some(sym);
            }
        }
        let side_key = same_name_generic_interface_key(key.as_ref());
        scope
            .symbols
            .get(&side_key)
            .filter(|sym| is_named_interface_like_symbol(sym))
            .filter(|sym| sym.name.eq_ignore_ascii_case(key.as_ref()))
    }

    pub(crate) fn named_interface_facet_symbol_in_scope(
        &self,
        scope_id: ScopeId,
        name: &str,
    ) -> Option<&Symbol> {
        let key = ensure_ascii_lowercase(name);
        let side_key = same_name_generic_interface_key(key.as_ref());
        self.scopes[scope_id]
            .symbols
            .get(&side_key)
            .filter(|symbol| is_named_interface_like_symbol(symbol))
            .or_else(|| self.named_interface_symbol_in_scope(scope_id, key.as_ref()))
    }

    pub fn may_have_named_interface_name(&self, name: &str) -> bool {
        let key = ensure_ascii_lowercase(name);
        if let Some(cached) = self
            .named_interface_presence_cache
            .borrow()
            .get(key.as_ref())
        {
            return *cached;
        }

        let found = self.scopes.iter().any(|scope| {
            self.named_interface_symbol_in_scope(scope.id, key.as_ref())
                .is_some()
                || scope
                    .use_associations
                    .iter()
                    .any(|assoc| assoc.local_name == key.as_ref())
        });
        self.named_interface_presence_cache
            .borrow_mut()
            .insert(key.into_owned(), found);
        found
    }

    /// Define a symbol in a specific scope.
    pub fn define_in(&mut self, scope_id: ScopeId, symbol: Symbol) -> Result<(), SemaError> {
        self.clear_lookup_caches();
        let key = symbol.name.to_lowercase();
        let scope = &mut self.scopes[scope_id];
        if scope.symbols.contains_key(&key) {
            return Err(SemaError {
                span: symbol.defined_at,
                msg: format!("symbol '{}' already defined in this scope", symbol.name),
            });
        }
        let mut symbol = symbol;
        symbol.scope = scope_id;
        if let Some(access) = scope.pending_access.get(&key).copied() {
            symbol.attrs.access = access;
        }
        scope.symbols.insert(key, symbol);
        Ok(())
    }

    /// Look up a name in the current scope with Fortran resolution order:
    /// Local > USE association > Host association > Implicit typing
    pub fn lookup(&self, name: &str) -> Option<&Symbol> {
        self.lookup_in(self.current, name)
    }

    /// Look up a name starting from a specific scope.
    pub fn lookup_in(&self, scope_id: ScopeId, name: &str) -> Option<&Symbol> {
        // Sprint 07: avoid the unconditional `to_ascii_lowercase`
        // allocation. Symtab keys live in canonical lowercase, but
        // most callers (lowering, type-spec resolution) already pass
        // pre-canonicalized names — borrow when there's nothing to
        // fold, allocate only when uppercase bytes are present.
        let key = ensure_ascii_lowercase(name);
        let mut visited = Vec::new();
        let mut cache = HashMap::new();
        self.lookup_in_guarded(scope_id, key.as_ref(), &mut visited, &mut cache)
    }

    /// Return the distinct providers that make a referenced local name
    /// ambiguous through USE association. Ordinary lookup remains first-match
    /// for resolution-time compatibility; validation calls this query before
    /// lowering and rejects ambiguous references.
    pub fn use_ambiguity_in(
        &self,
        scope_id: ScopeId,
        name: &str,
        allow_generic_merge: bool,
    ) -> Option<UseAmbiguity> {
        let key = ensure_ascii_lowercase(name);
        let mut visited = Vec::new();
        self.use_ambiguity_in_guarded(
            scope_id,
            key.as_ref(),
            LookupMode::Normal,
            allow_generic_merge,
            &mut visited,
        )
    }

    /// Check an explicit set of USE associations that is not represented by
    /// a symbol-table scope, such as the specification part of a BLOCK.
    pub fn use_ambiguity_from_associations(
        &self,
        origin_scope: ScopeId,
        name: &str,
        associations: &[UseAssociation],
        allow_generic_merge: bool,
    ) -> Option<UseAmbiguity> {
        let key = ensure_ascii_lowercase(name);
        let mut candidates = Vec::new();
        let mut visited = Vec::new();

        for assoc in associations {
            if assoc.local_name != key {
                continue;
            }
            let source_mode = if assoc.is_submodule_access {
                LookupMode::Normal
            } else {
                LookupMode::Exported
            };
            if let Some(ambiguity) = self.use_ambiguity_in_guarded(
                assoc.source_scope,
                &assoc.original_name,
                source_mode,
                allow_generic_merge,
                &mut visited,
            ) {
                return Some(ambiguity);
            }
            if let Some(symbol) = self.resolve_use_candidate(assoc) {
                let generic_facet = allow_generic_merge
                    && self.scope_has_generic_facet(
                        assoc.source_scope,
                        &assoc.original_name,
                        source_mode,
                    );
                self.push_use_candidate(&mut candidates, symbol, generic_facet);
            }
        }

        self.ambiguity_from_candidates(origin_scope, candidates, allow_generic_merge)
    }

    pub fn use_associations_bind_name(&self, associations: &[UseAssociation], name: &str) -> bool {
        let key = ensure_ascii_lowercase(name);
        associations
            .iter()
            .any(|assoc| assoc.local_name == key && self.resolve_use_candidate(assoc).is_some())
    }

    pub(crate) fn use_associations_conflict_with_local(
        &self,
        associations: &[UseAssociation],
        local_is_generic: bool,
    ) -> bool {
        let mut binding = DirectUseBinding::default();
        for association in associations {
            if association.is_submodule_access {
                continue;
            }
            let has_entity = self.resolve_use_candidate(association).is_some();
            let has_generic = self.scope_has_generic_facet(
                association.source_scope,
                &association.original_name,
                LookupMode::Exported,
            );
            binding.generic |= has_generic;
            binding.non_generic |= has_entity && !has_generic;
        }
        binding.non_generic || (binding.generic && !local_is_generic)
    }

    fn use_ambiguity_in_guarded(
        &self,
        scope_id: ScopeId,
        key: &str,
        mode: LookupMode,
        allow_generic_merge: bool,
        visited: &mut Vec<(ScopeId, String, LookupMode, bool)>,
    ) -> Option<UseAmbiguity> {
        let cache_key = (scope_id, key.to_string(), mode, allow_generic_merge);
        if let Some(cached) = self.use_ambiguity_cache.borrow().get(&cache_key) {
            return cached.clone();
        }
        if visited.contains(&cache_key) {
            return None;
        }
        visited.push(cache_key.clone());

        let result = self.compute_use_ambiguity(scope_id, key, mode, allow_generic_merge, visited);

        visited.pop();
        self.use_ambiguity_cache
            .borrow_mut()
            .insert(cache_key, result.clone());
        result
    }

    fn compute_use_ambiguity(
        &self,
        scope_id: ScopeId,
        key: &str,
        mode: LookupMode,
        allow_generic_merge: bool,
        visited: &mut Vec<(ScopeId, String, LookupMode, bool)>,
    ) -> Option<UseAmbiguity> {
        if mode == LookupMode::Exported && !self.scope_exports_key(scope_id, key) {
            return None;
        }

        let scope = &self.scopes[scope_id];
        if scope.symbols.contains_key(key)
            || self
                .named_interface_symbol_in_scope(scope_id, key)
                .is_some()
        {
            return None;
        }

        let mut candidates = Vec::new();
        for assoc in &scope.use_associations {
            if assoc.local_name != key {
                continue;
            }
            if !self.association_allowed_from_scope(scope_id, assoc, key) {
                continue;
            }
            if assoc.from_bare_use
                && assoc.local_name == assoc.original_name
                && self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key)
            {
                continue;
            }
            let source_mode = if assoc.is_submodule_access {
                LookupMode::Normal
            } else {
                LookupMode::Exported
            };
            if let Some(ambiguity) = self.use_ambiguity_in_guarded(
                assoc.source_scope,
                &assoc.original_name,
                source_mode,
                allow_generic_merge,
                visited,
            ) {
                return Some(ambiguity);
            }
            if let Some(symbol) = self.resolve_use_candidate(assoc) {
                let generic_facet = allow_generic_merge
                    && self.scope_has_generic_facet(
                        assoc.source_scope,
                        &assoc.original_name,
                        source_mode,
                    );
                self.push_use_candidate(&mut candidates, symbol, generic_facet);
            }
        }

        let mut seen_use_scopes = Vec::new();
        for assoc in &scope.use_associations {
            if !assoc.from_bare_use
                || assoc.local_name != assoc.original_name
                || seen_use_scopes.contains(&assoc.source_scope)
                || self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key)
                || !self.association_allowed_from_scope(scope_id, assoc, key)
            {
                continue;
            }
            seen_use_scopes.push(assoc.source_scope);
            let source_mode = if assoc.is_submodule_access {
                LookupMode::Normal
            } else {
                LookupMode::Exported
            };
            if let Some(ambiguity) = self.use_ambiguity_in_guarded(
                assoc.source_scope,
                key,
                source_mode,
                allow_generic_merge,
                visited,
            ) {
                return Some(ambiguity);
            }
            let mut lookup_visited = Vec::new();
            let mut lookup_cache = HashMap::new();
            let symbol = if assoc.is_submodule_access {
                self.lookup_in_guarded(
                    assoc.source_scope,
                    key,
                    &mut lookup_visited,
                    &mut lookup_cache,
                )
            } else {
                self.lookup_exported_in_guarded(
                    assoc.source_scope,
                    key,
                    &mut lookup_visited,
                    &mut lookup_cache,
                )
            };
            if let Some(symbol) = symbol {
                let generic_facet = allow_generic_merge
                    && self.scope_has_generic_facet(assoc.source_scope, key, source_mode);
                self.push_use_candidate(&mut candidates, symbol, generic_facet);
            }
        }

        let has_use_candidate = !candidates.is_empty();
        if let Some(ambiguity) =
            self.ambiguity_from_candidates(scope_id, candidates, allow_generic_merge)
        {
            return Some(ambiguity);
        }

        if !has_use_candidate && mode == LookupMode::Normal && scope.host_association.allows(key) {
            if let Some(parent) = scope.parent {
                if self.scopes[parent].kind != ScopeKind::Global {
                    return self.use_ambiguity_in_guarded(
                        parent,
                        key,
                        LookupMode::Normal,
                        allow_generic_merge,
                        visited,
                    );
                }
            }
        }
        None
    }

    fn ambiguity_from_candidates(
        &self,
        origin_scope: ScopeId,
        candidates: Vec<UseCandidate>,
        allow_generic_merge: bool,
    ) -> Option<UseAmbiguity> {
        if candidates.len() < 2
            || (allow_generic_merge && candidates.iter().all(|candidate| candidate.generic_facet))
        {
            return None;
        }
        let mut providers: Vec<String> = candidates
            .into_iter()
            .map(|candidate| candidate.provider)
            .collect();
        providers.sort();
        Some(UseAmbiguity {
            origin_scope,
            providers,
        })
    }

    fn resolve_use_candidate(&self, assoc: &UseAssociation) -> Option<&Symbol> {
        if assoc.is_submodule_access {
            return self.scopes[assoc.source_scope]
                .symbols
                .get(&assoc.original_name)
                .or_else(|| self.lookup_in(assoc.source_scope, &assoc.original_name));
        }
        let mut visited = Vec::new();
        let mut cache = HashMap::new();
        self.lookup_exported_in_guarded(
            assoc.source_scope,
            &assoc.original_name,
            &mut visited,
            &mut cache,
        )
    }

    fn use_name_is_fully_renamed(
        &self,
        scope_id: ScopeId,
        source_scope: ScopeId,
        key: &str,
    ) -> bool {
        let associations = &self.scopes[scope_id].use_associations;
        let bare_edges = associations
            .iter()
            .filter(|assoc| {
                assoc.source_scope == source_scope
                    && assoc.from_bare_use
                    && assoc.local_name.is_empty()
                    && assoc.original_name.is_empty()
            })
            .count();
        if bare_edges == 0 {
            return false;
        }
        let bare_renames = associations
            .iter()
            .filter(|assoc| {
                assoc.source_scope == source_scope
                    && assoc.from_bare_use
                    && assoc.local_name != assoc.original_name
                    && assoc.original_name == key
            })
            .count();
        bare_renames >= bare_edges
    }

    fn push_use_candidate(
        &self,
        candidates: &mut Vec<UseCandidate>,
        symbol: &Symbol,
        generic_facet: bool,
    ) {
        let Some(location) = self.locate_symbol(symbol) else {
            return;
        };
        let identity = match (&symbol.kind, symbol.attrs.type_owner_module.as_deref()) {
            (SymbolKind::DerivedType, Some(owner_module)) => UseEntityIdentity::DerivedType {
                owner_module: owner_module.to_ascii_lowercase(),
                name: symbol.name.to_ascii_lowercase(),
            },
            _ => UseEntityIdentity::Location(location.clone()),
        };
        if let Some(existing) = candidates
            .iter_mut()
            .find(|candidate| candidate.identity == identity)
        {
            existing.generic_facet |= generic_facet;
            return;
        }
        let provider = match &self.scopes[location.scope_id].kind {
            ScopeKind::Module(name) | ScopeKind::Submodule(name) => name.to_ascii_lowercase(),
            _ => format!("scope#{}", location.scope_id),
        };
        candidates.push(UseCandidate {
            identity,
            provider,
            generic_facet,
        });
    }

    fn scope_has_generic_facet(&self, scope_id: ScopeId, key: &str, mode: LookupMode) -> bool {
        let mut visited = Vec::new();
        self.scope_has_generic_facet_guarded(scope_id, key, mode, &mut visited)
    }

    fn scope_has_generic_facet_guarded(
        &self,
        scope_id: ScopeId,
        key: &str,
        mode: LookupMode,
        visited: &mut Vec<(ScopeId, String, LookupMode)>,
    ) -> bool {
        let visit_key = (scope_id, key.to_string(), mode);
        if visited.contains(&visit_key) {
            return false;
        }
        if mode == LookupMode::Exported && !self.scope_exports_key(scope_id, key) {
            return false;
        }
        visited.push(visit_key);

        let scope = &self.scopes[scope_id];
        if let Some(symbol) = self.named_interface_symbol_in_scope(scope_id, key) {
            let visible = mode == LookupMode::Normal || symbol_exports(symbol, scope);
            visited.pop();
            return visible;
        }
        if scope.symbols.contains_key(key) {
            visited.pop();
            return false;
        }

        let mut saw_binding = false;
        for assoc in &scope.use_associations {
            if assoc.local_name != key {
                continue;
            }
            if !self.association_allowed_from_scope(scope_id, assoc, key) {
                continue;
            }
            if assoc.from_bare_use
                && assoc.local_name == assoc.original_name
                && self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key)
            {
                continue;
            }
            saw_binding = self.resolve_use_candidate(assoc).is_some() || saw_binding;
            let source_mode = if assoc.is_submodule_access {
                LookupMode::Normal
            } else {
                LookupMode::Exported
            };
            if self.scope_has_generic_facet_guarded(
                assoc.source_scope,
                &assoc.original_name,
                source_mode,
                visited,
            ) {
                visited.pop();
                return true;
            }
        }

        let mut seen_use_scopes = Vec::new();
        for assoc in &scope.use_associations {
            if !assoc.from_bare_use
                || assoc.local_name != assoc.original_name
                || seen_use_scopes.contains(&assoc.source_scope)
                || self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key)
                || !self.association_allowed_from_scope(scope_id, assoc, key)
            {
                continue;
            }
            seen_use_scopes.push(assoc.source_scope);
            let mut lookup_visited = Vec::new();
            let mut lookup_cache = HashMap::new();
            let source_mode = if assoc.is_submodule_access {
                LookupMode::Normal
            } else {
                LookupMode::Exported
            };
            saw_binding |= if assoc.is_submodule_access {
                self.lookup_in_guarded(
                    assoc.source_scope,
                    key,
                    &mut lookup_visited,
                    &mut lookup_cache,
                )
            } else {
                self.lookup_exported_in_guarded(
                    assoc.source_scope,
                    key,
                    &mut lookup_visited,
                    &mut lookup_cache,
                )
            }
            .is_some();
            if self.scope_has_generic_facet_guarded(assoc.source_scope, key, source_mode, visited) {
                visited.pop();
                return true;
            }
        }

        let result = if !saw_binding
            && mode == LookupMode::Normal
            && self.host_association_allows_generic_facet(scope_id, key)
        {
            scope
                .parent
                .filter(|parent| self.scopes[*parent].kind != ScopeKind::Global)
                .is_some_and(|parent| {
                    self.scope_has_generic_facet_guarded(parent, key, LookupMode::Normal, visited)
                })
        } else {
            false
        };
        visited.pop();
        result
    }

    fn lookup_in_guarded<'a>(
        &'a self,
        scope_id: ScopeId,
        key: &str,
        visited: &mut Vec<(ScopeId, String, LookupMode)>,
        cache: &mut HashMap<(ScopeId, String, LookupMode), Option<&'a Symbol>>,
    ) -> Option<&'a Symbol> {
        if let Some(cached) = self.cached_lookup(LookupMode::Normal, scope_id, key) {
            return cached;
        }
        let visit_key = (scope_id, key.to_string(), LookupMode::Normal);
        if let Some(cached) = cache.get(&visit_key) {
            return *cached;
        }
        if visited.contains(&visit_key) {
            return None;
        }
        visited.push(visit_key.clone());

        let scope = &self.scopes[scope_id];

        let result = (|| {
            // 1. Local declaration.
            if let Some(sym) = scope.symbols.get(key) {
                return Some(sym);
            }

            // 2. Direct USE association — check the source module's own
            // symbols first, then chase through that module's USE chain
            // for the SAME name (handles re-exports like `use
            // stdlib_kinds, only: int32` where int32 itself is a USE-
            // associated re-export from iso_fortran_env). Only the UA's
            // original_name is followed, so unrelated names cannot leak.
            for assoc in &scope.use_associations {
                if assoc.local_name == key {
                    if !self.association_allowed_from_scope(scope_id, assoc, key) {
                        continue;
                    }
                    if assoc.from_bare_use
                        && assoc.local_name == assoc.original_name
                        && self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key)
                    {
                        continue;
                    }
                    if assoc.is_submodule_access {
                        if let Some(sym) = self.scopes[assoc.source_scope]
                            .symbols
                            .get(&assoc.original_name)
                        {
                            return Some(sym);
                        }
                        if let Some(sym) = self.lookup_in_guarded(
                            assoc.source_scope,
                            &assoc.original_name,
                            visited,
                            cache,
                        ) {
                            return Some(sym);
                        }
                    } else if let Some(sym) = self.lookup_exported_in_guarded(
                        assoc.source_scope,
                        &assoc.original_name,
                        visited,
                        cache,
                    ) {
                        return Some(sym);
                    }
                }
            }

            // 2b. Transitive USE: look through each USE'd module's own
            // public symbols and its transitive USE chain. Only applies
            // to bare `USE M` — `use M, only: x` and `use M, only: x =>
            // y` must NOT expose other names from M, including
            // same-named generic interfaces whose specifics would
            // otherwise be silently merged into a user-scope generic of
            // the same name.
            let mut seen_use_scopes = Vec::new();
            for assoc in &scope.use_associations {
                if !assoc.from_bare_use {
                    continue;
                }
                if !self.association_allowed_from_scope(scope_id, assoc, key) {
                    continue;
                }
                if assoc.local_name != assoc.original_name {
                    continue;
                }
                if self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key) {
                    continue;
                }
                if seen_use_scopes.contains(&assoc.source_scope) {
                    continue;
                }
                seen_use_scopes.push(assoc.source_scope);
                let symbol = if assoc.is_submodule_access {
                    self.lookup_in_guarded(assoc.source_scope, key, visited, cache)
                } else {
                    self.lookup_exported_in_guarded(assoc.source_scope, key, visited, cache)
                };
                if let Some(sym) = symbol {
                    return Some(sym);
                }
            }

            // 3. Host association — look in parent scope.
            if scope.host_association.allows(key) {
                if let Some(parent) = scope.parent {
                    if self.scopes[parent].kind != ScopeKind::Global {
                        if let Some(symbol) = self.lookup_in_guarded(parent, key, visited, cache) {
                            if self.host_association_allows_symbol(scope_id, key, symbol) {
                                return Some(symbol);
                            }
                        }
                    }
                }
            }

            None
        })();

        visited.pop();
        cache.insert(visit_key, result);
        self.remember_lookup(LookupMode::Normal, scope_id, key, result);
        result
    }

    pub fn scope_exports_name(&self, scope_id: ScopeId, name: &str) -> bool {
        let key = ensure_ascii_lowercase(name);
        self.scope_exports_key(scope_id, key.as_ref())
    }

    fn scope_exports_key(&self, scope_id: ScopeId, key: &str) -> bool {
        let scope = &self.scopes[scope_id];
        let mut saw_symbol = false;
        let mut exports = false;
        if let Some(sym) = scope.symbols.get(key) {
            saw_symbol = true;
            exports |= symbol_exports(sym, scope);
        }
        if let Some(sym) = self.named_interface_symbol_in_scope(scope_id, key) {
            saw_symbol = true;
            exports |= symbol_exports(sym, scope);
        }
        if saw_symbol {
            return exports;
        }
        if let Some(access) = scope.pending_access.get(key).copied() {
            return match access {
                Access::Public => true,
                Access::Private => false,
                Access::Default => !matches!(scope.default_access, Access::Private),
            };
        }

        let has_import_edge = scope.use_associations.iter().any(|assoc| {
            assoc.local_name == key || (assoc.from_bare_use && assoc.local_name.is_empty())
        });
        if !has_import_edge {
            return false;
        }

        match scope.default_access {
            Access::Public => true,
            Access::Private => false,
            Access::Default => !matches!(scope.default_access, Access::Private),
        }
    }

    fn lookup_exported_in_guarded<'a>(
        &'a self,
        scope_id: ScopeId,
        key: &str,
        visited: &mut Vec<(ScopeId, String, LookupMode)>,
        cache: &mut HashMap<(ScopeId, String, LookupMode), Option<&'a Symbol>>,
    ) -> Option<&'a Symbol> {
        if let Some(cached) = self.cached_lookup(LookupMode::Exported, scope_id, key) {
            return cached;
        }
        let visit_key = (scope_id, key.to_string(), LookupMode::Exported);
        if let Some(cached) = cache.get(&visit_key) {
            return *cached;
        }
        if visited.contains(&visit_key) {
            return None;
        }
        if !self.scope_exports_key(scope_id, key) {
            cache.insert(visit_key, None);
            self.remember_lookup(LookupMode::Exported, scope_id, key, None);
            return None;
        }
        visited.push(visit_key.clone());

        let scope = &self.scopes[scope_id];
        let result = (|| {
            if let Some(sym) = scope.symbols.get(key) {
                return Some(sym);
            }

            for assoc in &scope.use_associations {
                if assoc.local_name == key {
                    if !self.association_allowed_from_scope(scope_id, assoc, key) {
                        continue;
                    }
                    if assoc.from_bare_use
                        && assoc.local_name == assoc.original_name
                        && self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key)
                    {
                        continue;
                    }
                    if assoc.is_submodule_access {
                        if let Some(sym) = self.scopes[assoc.source_scope]
                            .symbols
                            .get(&assoc.original_name)
                        {
                            return Some(sym);
                        }
                        if let Some(sym) = self.lookup_in_guarded(
                            assoc.source_scope,
                            &assoc.original_name,
                            visited,
                            cache,
                        ) {
                            return Some(sym);
                        }
                    } else if let Some(sym) = self.lookup_exported_in_guarded(
                        assoc.source_scope,
                        &assoc.original_name,
                        visited,
                        cache,
                    ) {
                        return Some(sym);
                    }
                }
            }

            let mut seen_use_scopes = Vec::new();
            for assoc in &scope.use_associations {
                if !assoc.from_bare_use {
                    continue;
                }
                if !self.association_allowed_from_scope(scope_id, assoc, key) {
                    continue;
                }
                if assoc.local_name != assoc.original_name {
                    continue;
                }
                if self.use_name_is_fully_renamed(scope_id, assoc.source_scope, key) {
                    continue;
                }
                if seen_use_scopes.contains(&assoc.source_scope) {
                    continue;
                }
                seen_use_scopes.push(assoc.source_scope);
                let symbol = if assoc.is_submodule_access {
                    self.lookup_in_guarded(assoc.source_scope, key, visited, cache)
                } else {
                    self.lookup_exported_in_guarded(assoc.source_scope, key, visited, cache)
                };
                if let Some(sym) = symbol {
                    return Some(sym);
                }
            }

            None
        })();

        visited.pop();
        cache.insert(visit_key, result);
        self.remember_lookup(LookupMode::Exported, scope_id, key, result);
        result
    }

    /// Tries `lookup_in(proc_scope_id, name)` first — that walks
    /// local → USE associations → transitive USE → host scope per
    /// Fortran's normal name resolution order. Unrestricted scopes retain
    /// the legacy all-scope fallback used by lowering. Restricted IMPORT
    /// scopes must not bypass their host-association policy on a miss.
    pub fn lookup_local_then_any(
        &self,
        proc_scope_id: Option<ScopeId>,
        name: &str,
    ) -> Option<&Symbol> {
        if let Some(scope_id) = proc_scope_id {
            if let Some(sym) = self.lookup_in(scope_id, name) {
                return Some(sym);
            }
            if self.host_association_is_restricted(scope_id) {
                return None;
            }
        }
        self.find_symbol_any_scope(name)
    }

    pub(crate) fn host_association_is_restricted(&self, scope_id: ScopeId) -> bool {
        let mut current = Some(scope_id);
        while let Some(id) = current {
            let scope = &self.scopes[id];
            let control = &scope.host_association;
            if control.host_declaration_cutoff.is_some()
                || !matches!(control.policy, HostAssociationPolicy::All)
            {
                return true;
            }
            current = scope
                .parent
                .filter(|parent| self.scopes[*parent].kind != ScopeKind::Global);
        }
        false
    }

    /// Search ALL scopes for a symbol by name.
    /// Used during lowering when the current scope may not be set correctly.
    /// Prefers parameter symbols (for kind resolution) but returns any match.
    pub fn find_symbol_any_scope(&self, name: &str) -> Option<&Symbol> {
        let key_cow = ensure_ascii_lowercase(name);
        let key: &str = key_cow.as_ref();
        // Track the best fallback seen so far. A typed
        // Function/Subroutine carries the most useful information
        // (return type, kind, ABI) for callers that use this helper to
        // resolve a procedure reference. A NamedInterface with the same
        // name (common when a stdlib module re-exports a function via a
        // generic interface block) shadows the typed entry on the first
        // scope-iteration hit but provides only a list of specifics —
        // not enough for return-type or character-ABI lookup. Prefer
        // typed callable kinds over NamedInterface so callers don't
        // have to walk every scope themselves.
        let mut fallback: Option<&Symbol> = None;
        let mut typed_callable: Option<&Symbol> = None;
        for scope in &self.scopes {
            if let Some(sym) = scope.symbols.get(key) {
                if sym.attrs.parameter {
                    return Some(sym);
                }
                if matches!(
                    sym.kind,
                    SymbolKind::Function
                        | SymbolKind::Subroutine
                        | SymbolKind::ExternalProc
                        | SymbolKind::IntrinsicProc
                        | SymbolKind::ProcedurePointer
                ) && typed_callable.is_none()
                {
                    typed_callable = Some(sym);
                }
                if fallback.is_none() {
                    fallback = Some(sym);
                }
            }
        }
        if let Some(sym) = typed_callable {
            return Some(sym);
        }
        if fallback.is_some() {
            return fallback;
        }
        // Second pass: resolve USE renames. `use m, only: a => add`
        // installs a UseAssociation with local_name="a" and
        // original_name="add" but no symbol named "a" on the
        // enclosing scope. Direct-symbol scans miss the rename; walk
        // every scope's UseAssociations and follow the source to pick
        // up the underlying symbol (NamedInterface for generic
        // dispatch, Function for ordinary calls, etc.).
        //
        // The source-scope lookup MUST chase through transitive USE
        // chains: `stdlib_kinds` re-exports `int64` from
        // `iso_fortran_env`, so `use stdlib_kinds, only: block_kind => int64`
        // can't find `int64` in stdlib_kinds's own symbols — the kind
        // constant lives one hop further up. Without the chase,
        // `integer(block_kind) :: dummy` falls back to default kind=4
        // inside the submodule body, silently truncating the local
        // from 8 bytes to 4 even though the parent type's `block`
        // field is correctly laid out.
        for scope in &self.scopes {
            for assoc in &scope.use_associations {
                if assoc.local_name == key {
                    if let Some(sym) = self.scopes[assoc.source_scope]
                        .symbols
                        .get(&assoc.original_name)
                    {
                        return Some(sym);
                    }
                    if let Some(sym) = self.lookup_in(assoc.source_scope, &assoc.original_name) {
                        return Some(sym);
                    }
                }
            }
        }
        None
    }

    /// Check if a name would be implicitly typed in the current scope.
    /// Returns the implicit type if applicable, or None if implicit none.
    pub fn implicit_type(&self, name: &str) -> Option<ImplicitType> {
        let scope = &self.scopes[self.current];
        scope.implicit_rules.type_for(name)
    }

    /// Set implicit none for the current scope.
    pub fn set_implicit_none(&mut self, type_: bool, external: bool) {
        let scope = &mut self.scopes[self.current];
        scope.has_explicit_implicit_stmt = true;
        if type_ {
            scope.implicit_rules.none_type = true;
        }
        if external {
            scope.implicit_rules.none_external = true;
        }
    }

    /// Force IMPLICIT NONE on every program-unit-level scope in the
    /// table (Program, Module, Submodule, Subroutine, Function,
    /// BlockData).  Used by the driver's `-fimplicit-none` flag,
    /// which mirrors the gfortran option of the same name and tells
    /// validate.rs to flag every undeclared name even in scopes that
    /// don't have an explicit `implicit none` statement.
    pub fn force_implicit_none_all_units(&mut self) {
        for scope in &mut self.scopes {
            if matches!(
                scope.kind,
                ScopeKind::Program(_)
                    | ScopeKind::Module(_)
                    | ScopeKind::Submodule(_)
                    | ScopeKind::Subroutine(_)
                    | ScopeKind::Function(_)
            ) && !scope.has_explicit_implicit_stmt
            {
                scope.implicit_rules.none_type = true;
            }
        }
    }

    /// Set an implicit typing rule for the current scope.
    pub fn set_implicit_rule(&mut self, start: char, end: char, itype: ImplicitType) {
        let scope = &mut self.scopes[self.current];
        scope.has_explicit_implicit_stmt = true;
        scope.implicit_rules.none_type = false;
        for c in start..=end {
            scope
                .implicit_rules
                .rules
                .insert(c.to_ascii_lowercase(), itype);
        }
    }

    /// Add a USE association to the current scope.
    pub fn add_use_association(&mut self, assoc: UseAssociation) {
        self.clear_lookup_caches();
        let assoc = UseAssociation {
            local_name: assoc.local_name.to_ascii_lowercase(),
            original_name: assoc.original_name.to_ascii_lowercase(),
            source_scope: assoc.source_scope,
            is_submodule_access: assoc.is_submodule_access,
            from_bare_use: assoc.from_bare_use,
        };
        self.scopes[self.current].use_associations.push(assoc);
    }

    /// Set the default accessibility for the current scope.
    pub fn set_default_access(&mut self, access: Access) {
        self.clear_lookup_caches();
        self.scopes[self.current].default_access = access;
    }

    /// Set the access level on a specific symbol in the current scope.
    /// Used for `PUBLIC :: name` and `PRIVATE :: name` statements.
    pub fn set_symbol_access(&mut self, name: &str, access: Access) {
        self.clear_lookup_caches();
        let key = name.to_lowercase();
        self.scopes[self.current]
            .pending_access
            .insert(key.clone(), access);
        if let Some(sym) = self.scopes[self.current].symbols.get_mut(&key) {
            sym.attrs.access = access;
        }
        let side_key = same_name_generic_interface_key(&key);
        if let Some(sym) = self.scopes[self.current].symbols.get_mut(&side_key) {
            sym.attrs.access = access;
        }
    }

    /// Iterate all scopes (for generic interface resolution during lowering).
    pub fn all_scopes(&self) -> &[Scope] {
        &self.scopes
    }

    /// Check whether implicit none (type) is active in a scope.
    pub fn is_implicit_none(&self, scope_id: ScopeId) -> bool {
        self.scopes[scope_id].implicit_rules.none_type
    }

    /// Get the default accessibility for a scope.
    pub fn default_access(&self, scope_id: ScopeId) -> Access {
        self.scopes[scope_id].default_access
    }

    /// Find a module scope by name (for USE resolution within the same file).
    pub fn find_module_scope(&self, name: &str) -> Option<ScopeId> {
        self.scopes.iter().find_map(|s| {
            if let ScopeKind::Module(ref n) = s.kind {
                if n.eq_ignore_ascii_case(name) {
                    Some(s.id)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    pub fn find_submodule_scope(&self, ancestor: &str, name: &str) -> Option<ScopeId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| match &scope.kind {
                ScopeKind::Submodule(scope_name)
                    if scope_name.eq_ignore_ascii_case(name)
                        && scope
                            .submodule_ancestor
                            .as_deref()
                            .is_some_and(|value| value.eq_ignore_ascii_case(ancestor)) =>
                {
                    Some(scope.id)
                }
                _ => None,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostAssociationPolicy {
    All,
    None,
    Only(HashSet<String>),
}

impl HostAssociationPolicy {
    pub(crate) fn allows(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Only(names) => names.contains(name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostImportProtection {
    None,
    All,
    Names(HashSet<String>),
}

impl HostImportProtection {
    pub(crate) fn protects(&self, name: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Names(names) => names.contains(name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostAssociationControl {
    pub policy: HostAssociationPolicy,
    pub protection: HostImportProtection,
    pub host_declaration_cutoff: Option<Span>,
    pub host_scope_override: Option<ScopeId>,
}

impl HostAssociationControl {
    pub(crate) fn all() -> Self {
        Self {
            policy: HostAssociationPolicy::All,
            protection: HostImportProtection::None,
            host_declaration_cutoff: None,
            host_scope_override: None,
        }
    }

    fn allows(&self, name: &str) -> bool {
        self.policy.allows(name)
    }

    fn protects(&self, name: &str) -> bool {
        self.protection.protects(name)
    }
}

/// A scope in the symbol table.
#[derive(Debug)]
pub struct Scope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub kind: ScopeKind,
    pub symbols: HashMap<String, Symbol>,
    pub implicit_rules: ImplicitRules,
    pub has_explicit_implicit_stmt: bool,
    pub use_associations: Vec<UseAssociation>,
    pub(crate) host_association: HostAssociationControl,
    pub(crate) submodule_ancestor: Option<String>,
    pub default_access: Access,
    pub pending_access: HashMap<String, Access>,
    /// Ordered dummy argument names (for function/subroutine scopes).
    pub arg_order: Vec<String>,
}

/// What kind of scope this is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeKind {
    Global,
    Module(String),
    Submodule(String),
    Program(String),
    Subroutine(String),
    Function(String),
    Block,
    Interface,
    DerivedType(String),
    Forall,
    Associate,
    Critical,
}

/// A symbol — a named entity in a scope.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub type_info: Option<TypeInfo>,
    pub attrs: SymbolAttrs,
    pub defined_at: Span,
    pub scope: ScopeId,
    /// Ordered dummy argument names (for functions/subroutines).
    pub arg_names: Vec<String>,
    /// Compile-time constant value (for PARAMETERs like c_int=4).
    pub const_value: Option<i64>,
    /// Compile-time character value for character PARAMETERs.
    pub const_char_value: Option<String>,
}

/// What kind of entity this symbol represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Variable,
    Parameter,
    Function,
    Subroutine,
    Module,
    DerivedType,
    NamedInterface,
    Enumerator,
    /// F2023 enumeration type name (7.6.2) or named interoperable
    /// enum type (7.6.1). The symbol's type_info distinguishes them:
    /// Enumeration(name) for the strict kind, Integer for the
    /// interoperable alias.
    EnumerationType,
    Namelist,
    CommonBlock,
    ExternalProc,
    IntrinsicProc,
    ProcedurePointer,
    Label(u64),
}

/// Type information for a symbol.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeInfo {
    Integer {
        kind: Option<u8>,
    },
    Real {
        kind: Option<u8>,
    },
    DoublePrecision,
    Complex {
        kind: Option<u8>,
    },
    Logical {
        kind: Option<u8>,
    },
    Character {
        len: Option<i64>,
        kind: Option<u8>,
    },
    Derived(String),
    Class(String),
    ClassStar,
    TypeStar,
    /// F2023 enumeration type (7.6.2): name-based identity, strict —
    /// no implicit conversion to/from integers. Values lower to
    /// default-integer ordinals (1-based); all safety is frontend.
    Enumeration(String),
}

/// Symbol attributes.
#[derive(Debug, Clone)]
pub struct SymbolAttrs {
    pub access: Access,
    /// Defining module for a derived type. Imported type-layout closures may
    /// recreate the symbol in another module scope while retaining this
    /// canonical owner.
    pub type_owner_module: Option<String>,
    pub allocatable: bool,
    pub pointer: bool,
    /// For BIND(C, NAME="...") procedures, preserve the actual link
    /// symbol so lowering can call the declared external name rather
    /// than the local Fortran alias.
    pub binding_label: Option<String>,
    /// For `procedure(iface), pointer :: p`, preserve the declared
    /// interface name so `.amod` can round-trip the symbol truthfully.
    pub procedure_iface: Option<String>,
    pub target: bool,
    pub optional: bool,
    pub save: bool,
    pub parameter: bool,
    pub value: bool,
    pub intent: Option<Intent>,
    pub external: bool,
    pub intrinsic: bool,
    /// Procedure declared with the PURE prefix.
    pub pure: bool,
    /// Procedure declared with the ELEMENTAL prefix.
    pub elemental: bool,
    /// For Function symbols whose result is an array (allocatable,
    /// automatic, or fixed-shape): rank of the result.  0 for scalar
    /// results.  Used by lowering to route array-returning calls
    /// through the descriptor-return ABI even when the result isn't
    /// ALLOCATABLE — e.g. `real(sp), dimension(size(x)) :: w` is rank 1.
    pub result_rank: u8,
    /// Per-entity array specification — the same value the AST carries
    /// on `EntityDecl::array_spec` (or, when missing, derived from the
    /// `dimension(...)` attribute). Empty when the symbol is scalar.
    /// Sema populates this so consumers (notably SMP-body lowering)
    /// can recover full shape metadata without re-walking the AST decls.
    pub array_spec: Vec<ArraySpec>,
    /// Subroutine/Function declared with `module` prefix inside a
    /// submodule — the body of a separate module procedure declared
    /// in the parent module's interface block. Codegen links these
    /// under the parent module's name, not the submodule's, so call
    /// sites match `_afs_modproc_<parent>_<proc>`.
    pub is_separate_module_procedure: bool,
}

impl Default for SymbolAttrs {
    fn default() -> Self {
        Self {
            access: Access::Default,
            type_owner_module: None,
            allocatable: false,
            pointer: false,
            binding_label: None,
            procedure_iface: None,
            target: false,
            optional: false,
            save: false,
            parameter: false,
            value: false,
            intent: None,
            external: false,
            intrinsic: false,
            pure: false,
            elemental: false,
            result_rank: 0,
            array_spec: Vec::new(),
            is_separate_module_procedure: false,
        }
    }
}

/// Accessibility level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    Private,
    Default, // determined by module's default
}

/// Intent specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    In,
    Out,
    InOut,
}

/// USE association — links a local name to a symbol in another scope.
///
/// `from_bare_use` records that an association came from `use M` rather than
/// `use M, only: x`. Transitive lookup additionally requires the local and
/// original names to match, so a rename can retain its statement origin
/// without becoming a bare re-export edge.
#[derive(Debug, Clone)]
pub struct UseAssociation {
    pub local_name: String,
    pub original_name: String,
    pub source_scope: ScopeId,
    pub is_submodule_access: bool,
    pub from_bare_use: bool,
}

/// Implicit typing rules for a scope.
#[derive(Debug, Clone)]
pub struct ImplicitRules {
    pub none_type: bool,
    pub none_external: bool,
    pub rules: HashMap<char, ImplicitType>,
}

impl ImplicitRules {
    /// Standard Fortran default: I-N integer, everything else real.
    pub fn default_fortran() -> Self {
        let mut rules = HashMap::new();
        for c in 'a'..='h' {
            rules.insert(c, ImplicitType::Real);
        }
        for c in 'i'..='n' {
            rules.insert(c, ImplicitType::Integer);
        }
        for c in 'o'..='z' {
            rules.insert(c, ImplicitType::Real);
        }
        Self {
            none_type: false,
            none_external: false,
            rules,
        }
    }

    /// Look up the implicit type for a name's first letter.
    pub fn type_for(&self, name: &str) -> Option<ImplicitType> {
        if self.none_type {
            return None;
        }
        let first = name.chars().next()?.to_ascii_lowercase();
        self.rules.get(&first).copied()
    }
}

/// Implicit type assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImplicitType {
    Integer,
    Real,
    DoublePrecision,
    Complex,
    Logical,
    Character,
}

/// Semantic analysis error.
#[derive(Debug, Clone)]
pub struct SemaError {
    pub span: Span,
    pub msg: String,
}

impl std::fmt::Display for SemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}: error: {}",
            self.span.start.line, self.span.start.col, self.msg
        )
    }
}

impl std::error::Error for SemaError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{Position, Span};

    fn dummy_span() -> Span {
        Span {
            file_id: 0,
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    fn make_symbol(name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            name: name.into(),
            kind,
            type_info: None,
            attrs: SymbolAttrs::default(),
            defined_at: dummy_span(),
            scope: 0,
            arg_names: vec![],
            const_value: None,
            const_char_value: None,
        }
    }

    fn span_at(line: u32) -> Span {
        Span {
            file_id: 0,
            start: Position { line, col: 1 },
            end: Position { line, col: 1 },
        }
    }

    // ---- Basic scope operations ----

    #[test]
    fn define_and_lookup() {
        let mut st = SymbolTable::new();
        let scope_id = st.push_scope(ScopeKind::Program("main".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        let first = st.lookup("x").expect("first lookup");
        assert_eq!(first.scope, scope_id, "definition must record its owner");
        let second = st.lookup("x").expect("cached lookup");
        assert!(
            std::ptr::eq(first, second),
            "cache hits must preserve symbol identity"
        );
        {
            let cache = st.normal_lookup_cache.borrow();
            let location = cache
                .get(&scope_id)
                .and_then(|scope| scope.get("x"))
                .and_then(Option::as_ref)
                .expect("successful lookup must be cached");
            assert_eq!(location.scope_id, scope_id);
            assert_eq!(location.key, "x");
        }
        assert!(st.lookup("X").is_some()); // case insensitive
        assert!(st.lookup("y").is_none());
    }

    #[test]
    fn duplicate_definition_errors() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        assert!(st.define(make_symbol("x", SymbolKind::Variable)).is_err());
        assert!(st.define(make_symbol("X", SymbolKind::Variable)).is_err()); // case insensitive
    }

    #[test]
    fn case_insensitive_lookup() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.define(make_symbol("MyVar", SymbolKind::Variable))
            .unwrap();
        assert!(st.lookup("myvar").is_some());
        assert!(st.lookup("MYVAR").is_some());
        assert!(st.lookup("MyVar").is_some());
    }

    // ---- Host association ----

    #[test]
    fn host_association() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Subroutine("outer".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.push_scope(ScopeKind::Subroutine("inner".into()));
        // Inner sees outer's x via host association.
        assert!(st.lookup("x").is_some());
    }

    #[test]
    fn host_association_policy_filters_parent_and_invalidates_cache() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Subroutine("outer".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.define(make_symbol("y", SymbolKind::Variable)).unwrap();
        let inner = st.push_scope(ScopeKind::Subroutine("inner".into()));

        assert!(
            st.lookup("x").is_some(),
            "initial lookup populates the cache"
        );
        st.set_host_association_control(
            inner,
            HostAssociationControl {
                policy: HostAssociationPolicy::None,
                protection: HostImportProtection::None,
                host_declaration_cutoff: None,
                host_scope_override: None,
            },
        );
        assert!(st.lookup("x").is_none());
        assert!(st.lookup_local_then_any(Some(inner), "x").is_none());

        st.set_host_association_control(
            inner,
            HostAssociationControl {
                policy: HostAssociationPolicy::Only(HashSet::from(["x".into()])),
                protection: HostImportProtection::None,
                host_declaration_cutoff: None,
                host_scope_override: None,
            },
        );
        assert!(st.lookup("X").is_some());
        assert!(st.lookup("y").is_none());
    }

    #[test]
    fn host_declaration_cutoff_filters_direct_and_generic_facets_independently() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Subroutine("outer".into()));

        let mut early_direct = make_symbol("early_direct", SymbolKind::Function);
        early_direct.defined_at = span_at(1);
        st.define(early_direct).unwrap();
        let mut late_generic = make_symbol("early_direct", SymbolKind::NamedInterface);
        late_generic.defined_at = span_at(10);
        st.define_same_name_generic_interface(late_generic);

        let mut early_generic = make_symbol("early_generic", SymbolKind::NamedInterface);
        early_generic.defined_at = span_at(1);
        st.define_same_name_generic_interface(early_generic);
        let mut late_direct = make_symbol("early_generic", SymbolKind::Function);
        late_direct.defined_at = span_at(10);
        st.define(late_direct).unwrap();

        let inner = st.push_scope(ScopeKind::Subroutine("inner".into()));
        st.set_host_association_control(
            inner,
            HostAssociationControl {
                policy: HostAssociationPolicy::All,
                protection: HostImportProtection::None,
                host_declaration_cutoff: Some(span_at(5)),
                host_scope_override: None,
            },
        );

        assert!(st.lookup_in(inner, "early_direct").is_some());
        assert!(!st.scope_has_generic_facet(inner, "early_direct", LookupMode::Normal));
        assert!(st.lookup_in(inner, "early_generic").is_none());
        assert!(st.scope_has_generic_facet(inner, "early_generic", LookupMode::Normal));
    }

    #[test]
    fn explicit_host_imports_prevent_local_shadowing() {
        for protection in [
            HostImportProtection::All,
            HostImportProtection::Names(HashSet::from(["x".into()])),
        ] {
            let mut st = SymbolTable::new();
            st.push_scope(ScopeKind::Subroutine("outer".into()));
            st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
            let inner = st.push_scope(ScopeKind::Subroutine("inner".into()));
            st.set_host_association_control(
                inner,
                HostAssociationControl {
                    policy: HostAssociationPolicy::All,
                    protection,
                    host_declaration_cutoff: None,
                    host_scope_override: None,
                },
            );

            let err = st
                .define(make_symbol("x", SymbolKind::Variable))
                .expect_err("an explicit host import must prevent local shadowing");
            assert!(err.msg.contains("explicitly imported host entity"));
        }
    }

    #[test]
    fn host_association_survives_private_symbol_seen_on_use_branch() {
        let mut st = SymbolTable::new();

        let host_scope = st.push_scope(ScopeKind::Module("host".into()));
        let mut host_sym = make_symbol("color_red", SymbolKind::Parameter);
        host_sym.attrs.access = Access::Private;
        st.define(host_sym).unwrap();
        st.pop_scope();

        let imported_scope = st.push_scope(ScopeKind::Module("dep".into()));
        // Model a pathological search branch where transitive USE walks through a
        // scope whose parent is the eventual host scope. The private host symbol
        // must not poison the later host-association search.
        st.scope_mut(imported_scope).parent = Some(host_scope);
        st.pop_scope();

        st.push_scope(ScopeKind::Subroutine("inner".into()));
        st.scope_mut(st.current_scope()).parent = Some(host_scope);
        st.add_use_association(UseAssociation {
            local_name: "dep_item".into(),
            original_name: "dep_item".into(),
            source_scope: imported_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });

        assert!(
            st.lookup("color_red").is_some(),
            "host association should still find private host symbols even after a failed USE branch"
        );
    }

    #[test]
    fn local_shadows_host() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Subroutine("outer".into()));
        let mut outer_sym = make_symbol("x", SymbolKind::Variable);
        outer_sym.type_info = Some(TypeInfo::Integer { kind: None });
        st.define(outer_sym).unwrap();

        st.push_scope(ScopeKind::Subroutine("inner".into()));
        let mut inner_sym = make_symbol("x", SymbolKind::Variable);
        inner_sym.type_info = Some(TypeInfo::Real { kind: None });
        st.define(inner_sym).unwrap();

        // Inner's x shadows outer's x.
        let found = st.lookup("x").unwrap();
        assert!(matches!(found.type_info, Some(TypeInfo::Real { .. })));
    }

    // ---- USE association ----

    #[test]
    fn use_association() {
        let mut st = SymbolTable::new();

        // Create module scope with a public symbol.
        let mod_scope = st.push_scope(ScopeKind::Module("mymod".into()));
        st.define(make_symbol("foo", SymbolKind::Variable)).unwrap();
        st.pop_scope();

        // Create program scope that USEs the module.
        st.push_scope(ScopeKind::Program("main".into()));
        st.add_use_association(UseAssociation {
            local_name: "foo".into(),
            original_name: "foo".into(),
            source_scope: mod_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });

        assert!(st.lookup("foo").is_some());
    }

    #[test]
    fn pending_access_applies_to_late_defined_symbol() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Module("m".into()));
        st.set_default_access(Access::Private);
        st.set_symbol_access("create_list", Access::Public);

        let mut sym = make_symbol("create_list", SymbolKind::Function);
        sym.attrs.access = st.default_access(st.current_scope());
        st.define(sym).unwrap();

        let found = st.lookup("create_list").unwrap();
        assert_eq!(found.attrs.access, Access::Public);
    }

    #[test]
    fn use_rename() {
        let mut st = SymbolTable::new();

        let mod_scope = st.push_scope(ScopeKind::Module("mymod".into()));
        st.define(make_symbol("original_name", SymbolKind::Variable))
            .unwrap();
        st.pop_scope();

        st.push_scope(ScopeKind::Program("main".into()));
        st.add_use_association(UseAssociation {
            local_name: "local_name".into(),
            original_name: "original_name".into(),
            source_scope: mod_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });

        assert!(st.lookup("local_name").is_some());
        assert!(st.lookup("original_name").is_none()); // not accessible by original name
    }

    #[test]
    fn use_private_not_accessible() {
        let mut st = SymbolTable::new();

        let mod_scope = st.push_scope(ScopeKind::Module("mymod".into()));
        let mut sym = make_symbol("hidden", SymbolKind::Variable);
        sym.attrs.access = Access::Private;
        st.define(sym).unwrap();
        st.pop_scope();

        st.push_scope(ScopeKind::Program("main".into()));
        st.add_use_association(UseAssociation {
            local_name: "hidden".into(),
            original_name: "hidden".into(),
            source_scope: mod_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });

        assert!(st.lookup("hidden").is_none()); // private, not accessible
    }

    #[test]
    fn default_public_scope_does_not_export_unknown_name() {
        let mut st = SymbolTable::new();

        let mod_scope = st.push_scope(ScopeKind::Module("mymod".into()));
        assert!(
            !st.scope_exports_name(mod_scope, "missing"),
            "default PUBLIC governs declared or imported names, not arbitrary misses"
        );
    }

    #[test]
    fn default_public_scope_exports_imported_name() {
        let mut st = SymbolTable::new();

        let source_scope = st.push_scope(ScopeKind::Module("source".into()));
        st.define(make_symbol("exported", SymbolKind::Variable))
            .unwrap();
        st.pop_scope();

        let mod_scope = st.push_scope(ScopeKind::Module("mymod".into()));
        st.add_use_association(UseAssociation {
            local_name: "exported".into(),
            original_name: "exported".into(),
            source_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });

        assert!(
            st.scope_exports_name(mod_scope, "exported"),
            "default PUBLIC should still re-export imported names"
        );
    }

    #[test]
    fn cached_use_miss_is_invalidated_by_late_definition() {
        let mut st = SymbolTable::new();

        let mod_scope = st.push_scope(ScopeKind::Module("mymod".into()));
        st.pop_scope();

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        st.add_use_association(UseAssociation {
            local_name: "late".into(),
            original_name: "late".into(),
            source_scope: mod_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });

        assert!(st.lookup("late").is_none());

        st.enter_scope(mod_scope);
        st.define(make_symbol("late", SymbolKind::Variable))
            .unwrap();

        st.enter_scope(program_scope);
        assert!(st.lookup("late").is_some());
    }

    #[test]
    fn local_declaration_conflicts_with_use_association() {
        let mut st = SymbolTable::new();

        let mod_scope = st.push_scope(ScopeKind::Module("mymod".into()));
        let mut mod_sym = make_symbol("x", SymbolKind::Variable);
        mod_sym.type_info = Some(TypeInfo::Integer { kind: None });
        st.define(mod_sym).unwrap();
        st.pop_scope();

        st.push_scope(ScopeKind::Program("main".into()));
        st.add_use_association(UseAssociation {
            local_name: "x".into(),
            original_name: "x".into(),
            source_scope: mod_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });
        let mut local_sym = make_symbol("x", SymbolKind::Variable);
        local_sym.type_info = Some(TypeInfo::Real { kind: None });
        let err = st
            .define(local_sym)
            .expect_err("a local declaration must not shadow a USE-associated entity");
        assert!(err.msg.contains("USE-associated entity"));
    }

    #[test]
    fn distinct_use_entities_are_ambiguous_in_provider_order() {
        let mut st = SymbolTable::new();

        let z_scope = st.push_scope(ScopeKind::Module("z_provider".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();
        let a_scope = st.push_scope(ScopeKind::Module("a_provider".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        for source_scope in [z_scope, a_scope] {
            st.add_use_association(UseAssociation {
                local_name: "x".into(),
                original_name: "x".into(),
                source_scope,
                is_submodule_access: false,
                from_bare_use: false,
            });
        }

        assert_eq!(
            st.use_ambiguity_in(program_scope, "x", false),
            Some(UseAmbiguity {
                origin_scope: program_scope,
                providers: vec!["a_provider".into(), "z_provider".into()],
            })
        );
    }

    #[test]
    fn same_entity_reexported_by_two_modules_is_not_ambiguous() {
        let mut st = SymbolTable::new();

        let source_scope = st.push_scope(ScopeKind::Module("source".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();

        let mut facades = Vec::new();
        for name in ["left", "right"] {
            let facade_scope = st.push_scope(ScopeKind::Module(name.into()));
            st.add_use_association(UseAssociation {
                local_name: "x".into(),
                original_name: "x".into(),
                source_scope,
                is_submodule_access: false,
                from_bare_use: true,
            });
            st.pop_scope();
            facades.push(facade_scope);
        }

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        for source_scope in facades {
            st.add_use_association(UseAssociation {
                local_name: "x".into(),
                original_name: "x".into(),
                source_scope,
                is_submodule_access: false,
                from_bare_use: true,
            });
        }

        assert_eq!(st.use_ambiguity_in(program_scope, "x", false), None);
    }

    #[test]
    fn loaded_type_closures_with_the_same_owner_are_not_ambiguous() {
        let mut st = SymbolTable::new();
        let mut providers = Vec::new();

        for name in ["left", "right"] {
            let provider_scope = st.push_scope(ScopeKind::Module(name.into()));
            let mut symbol = make_symbol("item_t", SymbolKind::DerivedType);
            symbol.attrs.type_owner_module = Some("types".into());
            st.define(symbol).unwrap();
            st.pop_scope();
            providers.push(provider_scope);
        }

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        for source_scope in providers {
            st.add_use_association(UseAssociation {
                local_name: "item_t".into(),
                original_name: "item_t".into(),
                source_scope,
                is_submodule_access: false,
                from_bare_use: false,
            });
        }

        assert_eq!(st.use_ambiguity_in(program_scope, "item_t", false), None);
    }

    #[test]
    fn loaded_type_closures_with_distinct_owners_are_ambiguous() {
        let mut st = SymbolTable::new();
        let mut providers = Vec::new();

        for (name, owner) in [("left", "alpha"), ("right", "beta")] {
            let provider_scope = st.push_scope(ScopeKind::Module(name.into()));
            let mut symbol = make_symbol("item_t", SymbolKind::DerivedType);
            symbol.attrs.type_owner_module = Some(owner.into());
            st.define(symbol).unwrap();
            st.pop_scope();
            providers.push(provider_scope);
        }

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        for source_scope in providers {
            st.add_use_association(UseAssociation {
                local_name: "item_t".into(),
                original_name: "item_t".into(),
                source_scope,
                is_submodule_access: false,
                from_bare_use: false,
            });
        }

        assert!(st
            .use_ambiguity_in(program_scope, "item_t", false)
            .is_some());
    }

    #[test]
    fn adding_use_association_invalidates_ambiguity_cache() {
        let mut st = SymbolTable::new();

        let left_scope = st.push_scope(ScopeKind::Module("left".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();
        let right_scope = st.push_scope(ScopeKind::Module("right".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        st.add_use_association(UseAssociation {
            local_name: "x".into(),
            original_name: "x".into(),
            source_scope: left_scope,
            is_submodule_access: false,
            from_bare_use: false,
        });
        assert_eq!(st.use_ambiguity_in(program_scope, "x", false), None);

        st.add_use_association(UseAssociation {
            local_name: "x".into(),
            original_name: "x".into(),
            source_scope: right_scope,
            is_submodule_access: false,
            from_bare_use: false,
        });
        assert!(st.use_ambiguity_in(program_scope, "x", false).is_some());
    }

    #[test]
    fn local_declaration_cannot_suppress_use_ambiguity() {
        let mut st = SymbolTable::new();

        let left_scope = st.push_scope(ScopeKind::Module("left".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();
        let right_scope = st.push_scope(ScopeKind::Module("right".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        for source_scope in [left_scope, right_scope] {
            st.add_use_association(UseAssociation {
                local_name: "x".into(),
                original_name: "x".into(),
                source_scope,
                is_submodule_access: false,
                from_bare_use: false,
            });
        }
        let err = st
            .define(make_symbol("x", SymbolKind::Variable))
            .expect_err("a local declaration must not suppress a USE ambiguity");

        assert!(err.msg.contains("USE-associated entity"));
        assert!(st.use_ambiguity_in(program_scope, "x", false).is_some());
    }

    #[test]
    fn callable_use_generics_merge_but_value_references_are_ambiguous() {
        let mut st = SymbolTable::new();
        let mut modules = Vec::new();

        for name in ["left", "right"] {
            let module_scope = st.push_scope(ScopeKind::Module(name.into()));
            st.define(make_symbol("pick", SymbolKind::NamedInterface))
                .unwrap();
            st.pop_scope();
            modules.push(module_scope);
        }

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        for source_scope in modules {
            st.add_use_association(UseAssociation {
                local_name: "pick".into(),
                original_name: "pick".into(),
                source_scope,
                is_submodule_access: false,
                from_bare_use: false,
            });
        }

        assert!(st.use_ambiguity_in(program_scope, "pick", false).is_some());
        assert_eq!(st.use_ambiguity_in(program_scope, "pick", true), None);
    }

    #[test]
    fn bare_use_rename_does_not_make_remote_name_ambiguous() {
        let mut st = SymbolTable::new();

        let renamed_scope = st.push_scope(ScopeKind::Module("renamed".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();
        let direct_scope = st.push_scope(ScopeKind::Module("direct".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        st.pop_scope();

        let program_scope = st.push_scope(ScopeKind::Program("main".into()));
        for assoc in [
            UseAssociation {
                local_name: String::new(),
                original_name: String::new(),
                source_scope: renamed_scope,
                is_submodule_access: false,
                from_bare_use: true,
            },
            UseAssociation {
                local_name: "x".into(),
                original_name: "x".into(),
                source_scope: renamed_scope,
                is_submodule_access: false,
                from_bare_use: true,
            },
            UseAssociation {
                local_name: "y".into(),
                original_name: "x".into(),
                source_scope: renamed_scope,
                is_submodule_access: false,
                from_bare_use: true,
            },
            UseAssociation {
                local_name: String::new(),
                original_name: String::new(),
                source_scope: direct_scope,
                is_submodule_access: false,
                from_bare_use: true,
            },
            UseAssociation {
                local_name: "x".into(),
                original_name: "x".into(),
                source_scope: direct_scope,
                is_submodule_access: false,
                from_bare_use: true,
            },
        ] {
            st.add_use_association(assoc);
        }

        assert_eq!(st.use_ambiguity_in(program_scope, "x", false), None);
        assert_eq!(st.use_ambiguity_in(program_scope, "y", false), None);
    }

    // ---- Implicit typing ----

    #[test]
    fn implicit_default_rules() {
        let st = SymbolTable::new();
        // i-n → integer.
        assert_eq!(
            st.scopes[0].implicit_rules.type_for("index"),
            Some(ImplicitType::Integer)
        );
        assert_eq!(
            st.scopes[0].implicit_rules.type_for("jmax"),
            Some(ImplicitType::Integer)
        );
        // a-h, o-z → real.
        assert_eq!(
            st.scopes[0].implicit_rules.type_for("x"),
            Some(ImplicitType::Real)
        );
        assert_eq!(
            st.scopes[0].implicit_rules.type_for("alpha"),
            Some(ImplicitType::Real)
        );
    }

    #[test]
    fn implicit_none_disables() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.set_implicit_none(true, false);
        assert_eq!(st.implicit_type("x"), None);
        assert_eq!(st.implicit_type("index"), None);
    }

    #[test]
    fn implicit_custom_rules() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.set_implicit_rule('a', 'z', ImplicitType::DoublePrecision);
        assert_eq!(st.implicit_type("x"), Some(ImplicitType::DoublePrecision));
        assert_eq!(
            st.implicit_type("index"),
            Some(ImplicitType::DoublePrecision)
        );
    }

    // ---- Module scope finding ----

    #[test]
    fn find_module_scope() {
        let mut st = SymbolTable::new();
        let mod_id = st.push_scope(ScopeKind::Module("my_module".into()));
        st.pop_scope();
        assert_eq!(st.find_module_scope("my_module"), Some(mod_id));
        assert_eq!(st.find_module_scope("MY_MODULE"), Some(mod_id)); // case insensitive
        assert_eq!(st.find_module_scope("other"), None);
    }

    #[test]
    fn find_submodule_scope() {
        let mut st = SymbolTable::new();
        let first_id = st.push_scope(ScopeKind::Submodule("child".into()));
        st.set_submodule_ancestor(first_id, "first");
        st.pop_scope();
        let second_id = st.push_scope(ScopeKind::Submodule("child".into()));
        st.set_submodule_ancestor(second_id, "second");
        st.pop_scope();
        assert_eq!(st.find_submodule_scope("FIRST", "CHILD"), Some(first_id));
        assert_eq!(st.find_submodule_scope("second", "child"), Some(second_id));
        assert_eq!(st.find_submodule_scope("first", "other"), None);
    }

    // ---- Scope hierarchy ----

    #[test]
    fn scope_push_pop() {
        let mut st = SymbolTable::new();
        assert_eq!(st.current_scope(), 0); // global
        let s1 = st.push_scope(ScopeKind::Module("m".into()));
        assert_eq!(st.current_scope(), s1);
        let s2 = st.push_scope(ScopeKind::Subroutine("sub".into()));
        assert_eq!(st.current_scope(), s2);
        st.pop_scope();
        assert_eq!(st.current_scope(), s1);
        st.pop_scope();
        assert_eq!(st.current_scope(), 0);
    }

    // ---- Default access ----

    #[test]
    fn module_default_access() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Module("m".into()));
        assert_eq!(st.default_access(st.current_scope()), Access::Public);
        st.set_default_access(Access::Private);
        assert_eq!(st.default_access(st.current_scope()), Access::Private);
    }
}
