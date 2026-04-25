//! Symbol table infrastructure.
//!
//! Provides scope-based symbol management with Fortran's four association
//! mechanisms: local declaration, USE association, host association, and
//! IMPORT. Handles implicit typing and case-insensitive lookup.

use crate::lexer::Span;
use std::collections::HashMap;

/// Scope identifier — an index into the SymbolTable's scope list.
pub type ScopeId = usize;

/// The symbol table — manages all scopes in a compilation.
#[derive(Debug)]
pub struct SymbolTable {
    pub(crate) scopes: Vec<Scope>,
    pub(crate) current: ScopeId,
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
        }
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
        &mut self.scopes[id]
    }

    /// Define a symbol in the current scope.
    pub fn define(&mut self, symbol: Symbol) -> Result<(), SemaError> {
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

    /// Define a symbol in a specific scope.
    pub fn define_in(&mut self, scope_id: ScopeId, symbol: Symbol) -> Result<(), SemaError> {
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
        let key = name.to_ascii_lowercase();
        let mut visited = Vec::new();
        self.lookup_in_guarded(scope_id, &key, &mut visited)
    }

    fn lookup_in_guarded(
        &self,
        scope_id: ScopeId,
        key: &str,
        visited: &mut Vec<ScopeId>,
    ) -> Option<&Symbol> {
        if visited.contains(&scope_id) {
            return None;
        }
        visited.push(scope_id);

        let scope = &self.scopes[scope_id];

        let result = (|| {
            // 1. Local declaration.
            if let Some(sym) = scope.symbols.get(key) {
                return Some(sym);
            }

            // 2. Direct USE association.
            for assoc in &scope.use_associations {
                if assoc.local_name == key {
                    if let Some(sym) = self.scopes[assoc.source_scope]
                        .symbols
                        .get(&assoc.original_name)
                    {
                        if sym.attrs.access != Access::Private || assoc.is_submodule_access {
                            return Some(sym);
                        }
                    }
                }
            }

            // 2b. Transitive USE: look through each USE'd module's own
            // public symbols and its transitive USE chain. Only applies
            // to bare `USE M` (local_name == original_name); renamed
            // USE associations are intentional restrictions.
            let mut seen_use_scopes = Vec::new();
            for assoc in &scope.use_associations {
                if assoc.local_name != assoc.original_name {
                    continue;
                }
                if seen_use_scopes.contains(&assoc.source_scope) {
                    continue;
                }
                seen_use_scopes.push(assoc.source_scope);
                if let Some(sym) = self.lookup_in_guarded(assoc.source_scope, key, visited) {
                    if sym.attrs.access != Access::Private {
                        return Some(sym);
                    }
                }
            }

            // 3. Host association — look in parent scope.
            if let Some(parent) = scope.parent {
                if self.scopes[parent].kind != ScopeKind::Global {
                    return self.lookup_in_guarded(parent, key, visited);
                }
            }

            None
        })();

        visited.pop();
        result
    }

    /// Search ALL scopes for a symbol by name.
    /// Used during lowering when the current scope may not be set correctly.
    /// Prefers parameter symbols (for kind resolution) but returns any match.
    pub fn find_symbol_any_scope(&self, name: &str) -> Option<&Symbol> {
        let key = name.to_ascii_lowercase();
        let mut fallback: Option<&Symbol> = None;
        for scope in &self.scopes {
            if let Some(sym) = scope.symbols.get(&key) {
                if sym.attrs.parameter {
                    return Some(sym);
                }
                if fallback.is_none() {
                    fallback = Some(sym);
                }
            }
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
        for scope in &self.scopes {
            for assoc in &scope.use_associations {
                if assoc.local_name == key {
                    if let Some(sym) = self.scopes[assoc.source_scope]
                        .symbols
                        .get(&assoc.original_name)
                    {
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
        let assoc = UseAssociation {
            local_name: assoc.local_name.to_ascii_lowercase(),
            original_name: assoc.original_name.to_ascii_lowercase(),
            source_scope: assoc.source_scope,
            is_submodule_access: assoc.is_submodule_access,
        };
        self.scopes[self.current].use_associations.push(assoc);
    }

    /// Set the default accessibility for the current scope.
    pub fn set_default_access(&mut self, access: Access) {
        self.scopes[self.current].default_access = access;
    }

    /// Set the access level on a specific symbol in the current scope.
    /// Used for `PUBLIC :: name` and `PRIVATE :: name` statements.
    pub fn set_symbol_access(&mut self, name: &str, access: Access) {
        let key = name.to_lowercase();
        self.scopes[self.current]
            .pending_access
            .insert(key.clone(), access);
        if let Some(sym) = self.scopes[self.current].symbols.get_mut(&key) {
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
        let key = name.to_lowercase();
        self.scopes.iter().find_map(|s| {
            if let ScopeKind::Module(ref n) = s.kind {
                if n.to_lowercase() == key {
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
    Integer { kind: Option<u8> },
    Real { kind: Option<u8> },
    DoublePrecision,
    Complex { kind: Option<u8> },
    Logical { kind: Option<u8> },
    Character { len: Option<i64>, kind: Option<u8> },
    Derived(String),
    Class(String),
    ClassStar,
    TypeStar,
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
#[derive(Debug, Clone)]
pub struct UseAssociation {
    pub local_name: String,
    pub original_name: String,
    pub source_scope: ScopeId,
    pub is_submodule_access: bool,
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
        });

        assert!(st.lookup("hidden").is_none()); // private, not accessible
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
