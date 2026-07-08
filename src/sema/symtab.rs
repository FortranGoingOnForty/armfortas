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
use std::collections::HashMap;

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

#[derive(Clone, Debug)]
struct CachedSymbolRef {
    scope_id: ScopeId,
    key: String,
}

type PersistentLookupCache = HashMap<ScopeId, HashMap<String, Option<CachedSymbolRef>>>;

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
            default_access: Access::Public,
            pending_access: HashMap::new(),
            arg_order: Vec::new(),
        };
        Self {
            scopes: vec![global],
            current: 0,
            normal_lookup_cache: RefCell::new(HashMap::new()),
            export_lookup_cache: RefCell::new(HashMap::new()),
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
    }

    fn cached_lookup<'a>(
        &'a self,
        mode: LookupMode,
        scope_id: ScopeId,
        key: &str,
    ) -> Option<Option<&'a Symbol>> {
        let cached = self
            .lookup_cache(mode)
            .borrow()
            .get(&scope_id)
            .and_then(|scope_cache| scope_cache.get(key).cloned());
        cached.map(|loc| loc.and_then(|loc| self.symbol_at(&loc)))
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
        self.locate_symbol_in_scope(sym.scope, sym).or_else(|| {
            self.scopes
                .iter()
                .filter(|scope| scope.id != sym.scope)
                .find_map(|scope| self.locate_symbol_in_scope(scope.id, sym))
        })
    }

    fn locate_symbol_in_scope(&self, scope_id: ScopeId, sym: &Symbol) -> Option<CachedSymbolRef> {
        let scope = self.scopes.get(scope_id)?;
        scope.symbols.iter().find_map(|(key, candidate)| {
            std::ptr::eq(candidate, sym).then(|| CachedSymbolRef {
                scope_id,
                key: key.clone(),
            })
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

    /// Define a symbol in the current scope.
    pub fn define(&mut self, symbol: Symbol) -> Result<(), SemaError> {
        self.clear_lookup_caches();
        let key = symbol.name.to_lowercase();
        let scope = &mut self.scopes[self.current];
        if scope.symbols.contains_key(&key) {
            return Err(SemaError {
                span: symbol.defined_at,
                msg: format!("symbol '{}' already defined in this scope", symbol.name),
            });
        }
        let mut symbol = symbol;
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

    fn lookup_in_guarded<'a>(
        &'a self,
        scope_id: ScopeId,
        key: &str,
        visited: &mut Vec<(ScopeId, String, LookupMode)>,
        cache: &mut HashMap<(ScopeId, String, LookupMode), Option<&'a Symbol>>,
    ) -> Option<&'a Symbol> {
        let visit_key = (scope_id, key.to_string(), LookupMode::Normal);
        if let Some(cached) = self.cached_lookup(LookupMode::Normal, scope_id, key) {
            cache.insert(visit_key, cached);
            return cached;
        }
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
                if assoc.local_name != assoc.original_name {
                    continue;
                }
                if seen_use_scopes.contains(&assoc.source_scope) {
                    continue;
                }
                seen_use_scopes.push(assoc.source_scope);
                if let Some(sym) =
                    self.lookup_exported_in_guarded(assoc.source_scope, key, visited, cache)
                {
                    return Some(sym);
                }
            }

            // 3. Host association — look in parent scope.
            if let Some(parent) = scope.parent {
                if self.scopes[parent].kind != ScopeKind::Global {
                    return self.lookup_in_guarded(parent, key, visited, cache);
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
        match scope
            .pending_access
            .get(key)
            .copied()
            .unwrap_or(Access::Default)
        {
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
        let visit_key = (scope_id, key.to_string(), LookupMode::Exported);
        if let Some(cached) = self.cached_lookup(LookupMode::Exported, scope_id, key) {
            cache.insert(visit_key, cached);
            return cached;
        }
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
                if assoc.local_name != assoc.original_name {
                    continue;
                }
                if seen_use_scopes.contains(&assoc.source_scope) {
                    continue;
                }
                seen_use_scopes.push(assoc.source_scope);
                if let Some(sym) =
                    self.lookup_exported_in_guarded(assoc.source_scope, key, visited, cache)
                {
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

    /// Sprint 08: scope-correct lookup with all-scope fallback.
    ///
    /// Tries `lookup_in(proc_scope_id, name)` first — that walks
    /// local → USE associations → transitive USE → host scope per
    /// Fortran's normal name resolution order, returning the symbol
    /// the source actually refers to. If `proc_scope_id` is `None`
    /// (or the scoped lookup misses), falls back to
    /// `find_symbol_any_scope`, which scans every scope without
    /// regard to visibility.
    ///
    /// The fallback preserves prior behavior — this helper is a
    /// stepping stone, not a behavior change. Call sites in
    /// `src/ir/lower/**` should migrate to this helper as part of
    /// killing `find_symbol_any_scope` (see `feedback_lookup_discipline.md`).
    pub fn lookup_local_then_any(
        &self,
        proc_scope_id: Option<ScopeId>,
        name: &str,
    ) -> Option<&Symbol> {
        if let Some(scope_id) = proc_scope_id {
            if let Some(sym) = self.lookup_in(scope_id, name) {
                return Some(sym);
            }
        }
        self.find_symbol_any_scope(name)
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
/// `from_bare_use` distinguishes `use M` (true — full re-export visibility,
/// transitive lookup walks the source module's USE chain) from `use M, only:
/// x` (false — only the explicitly named symbols are visible). Without this
/// flag the transitive walk in `lookup_in_guarded` happily resolves any
/// generic interface in `M` even when only an unrelated name was imported,
/// silently merging foreign specifics into a same-named generic in the user
/// scope.
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

    // ---- Basic scope operations ----

    #[test]
    fn define_and_lookup() {
        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.define(make_symbol("x", SymbolKind::Variable)).unwrap();
        assert!(st.lookup("x").is_some());
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
    fn local_shadows_use() {
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
        st.define(local_sym).unwrap();

        // Local shadows USE.
        let found = st.lookup("x").unwrap();
        assert!(matches!(found.type_info, Some(TypeInfo::Real { .. })));
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
