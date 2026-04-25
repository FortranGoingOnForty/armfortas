//! Symbol resolution — walks the AST and populates symbol tables.
//!
//! First pass: collect declarations, create scopes, process USE/IMPLICIT.
//! This establishes the symbol table that type checking (Sprint 13) will use.

use super::symtab::*;
use crate::ast::decl;
use crate::ast::decl::{Attribute, Decl, OnlyItem, SpannedDecl, TypeSpec};
use crate::ast::unit::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

thread_local! {
    /// Track externally loaded module interfaces so resolve_file can
    /// return them to the driver for globals extraction.
    static LOADED_EXTERNAL_MODULES: RefCell<Vec<super::amod::ModuleInterface>> = const { RefCell::new(Vec::new()) };
}

fn merge_specific_names(into: &mut Vec<String>, additional: &[String]) {
    let mut seen: HashSet<String> = into.iter().map(|name| name.to_ascii_lowercase()).collect();
    for name in additional {
        let key = name.to_ascii_lowercase();
        if seen.insert(key) {
            into.push(name.clone());
        }
    }
}

fn merged_visible_generic_specifics(
    st: &SymbolTable,
    scope_id: ScopeId,
    generic_name: &str,
    local_specifics: &[String],
) -> Vec<String> {
    let mut merged = Vec::new();
    if let Some(existing) = st.lookup_in(scope_id, generic_name) {
        if existing.kind == SymbolKind::NamedInterface
            || (existing.kind == SymbolKind::DerivedType && !existing.arg_names.is_empty())
        {
            merge_specific_names(&mut merged, &existing.arg_names);
        }
    }
    merge_specific_names(&mut merged, local_specifics);
    merged
}

/// Walk a list of program units and build the symbol table.
/// Result of resolving a file: symbol table, type layouts, and any
/// external module interfaces loaded from .amod files during USE
/// resolution.
pub struct ResolveResult {
    pub st: SymbolTable,
    pub type_layouts: super::type_layout::TypeLayoutRegistry,
    pub external_modules: Vec<super::amod::ModuleInterface>,
}

pub fn resolve_file(
    units: &[SpannedUnit],
    module_search_paths: &[std::path::PathBuf],
) -> Result<ResolveResult, SemaError> {
    let mut st = SymbolTable::new();
    let mut layouts = super::type_layout::TypeLayoutRegistry::new();

    // Register intrinsic modules (iso_c_binding, iso_fortran_env) so USE can find them.
    super::intrinsic_modules::register_intrinsic_modules(&mut st);

    // First pass: create module scopes so USE can find them.
    for unit in units {
        if let ProgramUnit::Module { name, .. } = &unit.node {
            st.push_scope(ScopeKind::Module(name.clone()));
            st.pop_scope();
        }
    }

    // Second pass: populate all scopes (loads .amod files lazily on USE miss).
    // Track which external modules were loaded.
    LOADED_EXTERNAL_MODULES.with(|cell| cell.borrow_mut().clear());
    for unit in units {
        resolve_unit(&mut st, unit, module_search_paths, &mut layouts)?;
    }
    let external_modules = LOADED_EXTERNAL_MODULES.with(|cell| {
        let v = cell.borrow();
        v.iter().cloned().collect::<Vec<_>>()
    });

    // Third pass: compute layouts for all derived types.
    compute_all_layouts(units, &st, &mut layouts);

    Ok(ResolveResult {
        st,
        type_layouts: layouts,
        external_modules,
    })
}

fn backfill_procedure_pointer_interfaces(st: &mut SymbolTable, scope_id: ScopeId) {
    let updates: Vec<(String, Option<TypeInfo>, Vec<String>)> = st
        .scope(scope_id)
        .symbols
        .iter()
        .filter_map(|(key, sym)| {
            if sym.kind != SymbolKind::ProcedurePointer {
                return None;
            }
            let TypeInfo::Derived(iface_name) = sym.type_info.as_ref()? else {
                return None;
            };
            let iface_sym = st.find_symbol_any_scope(&iface_name.to_lowercase())?;
            Some((
                key.clone(),
                iface_sym.type_info.clone(),
                iface_sym.arg_names.clone(),
            ))
        })
        .collect();

    for (key, type_info, arg_names) in updates {
        if let Some(sym) = st.scope_mut(scope_id).symbols.get_mut(&key) {
            if let Some(type_info) = type_info {
                sym.type_info = Some(type_info);
            }
            sym.arg_names = arg_names;
        }
    }
}

fn backfill_function_result_type(
    st: &mut SymbolTable,
    host_scope: ScopeId,
    function_scope: ScopeId,
    function_name: &str,
    result_name: &str,
) {
    let result_key = result_name.to_ascii_lowercase();
    let (type_info, pointer, allocatable) = match st.scope(function_scope).symbols.get(&result_key)
    {
        Some(sym) => (sym.type_info.clone(), sym.attrs.pointer, sym.attrs.allocatable),
        None => return,
    };
    let Some(type_info) = type_info else {
        return;
    };
    let function_key = function_name.to_ascii_lowercase();
    if let Some(sym) = st.scope_mut(host_scope).symbols.get_mut(&function_key) {
        sym.type_info = Some(type_info);
        if pointer {
            sym.attrs.pointer = true;
        }
        if allocatable {
            sym.attrs.allocatable = true;
        }
    }
}

fn normalized_bind_name(
    bind: Option<&crate::ast::unit::BindInfo>,
    default_name: &str,
) -> Option<String> {
    bind.map(|info| {
        info.name
            .as_deref()
            .unwrap_or(default_name)
            .trim_matches('\'')
            .trim_matches('"')
            .to_string()
    })
}

type InterfaceOuterRef = (
    String,
    SymbolKind,
    Option<TypeInfo>,
    Vec<String>,
    Option<String>,
);

fn resolve_unit(
    st: &mut SymbolTable,
    unit: &SpannedUnit,
    module_search_paths: &[std::path::PathBuf],
    layouts: &mut super::type_layout::TypeLayoutRegistry,
) -> Result<(), SemaError> {
    match &unit.node {
        ProgramUnit::Program {
            name,
            uses,
            imports: _,
            implicit,
            decls,
            body,
            contains,
        } => {
            let scope_name = name.clone().unwrap_or_else(|| "<main>".into());
            st.push_scope(ScopeKind::Program(scope_name));
            let scope_id = st.current_scope();
            process_uses(st, uses, module_search_paths, layouts)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            preload_stmt_uses(st, body, module_search_paths, layouts);
            process_contains(st, contains, module_search_paths, layouts)?;
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::Module {
            name,
            uses,
            imports: _,
            implicit,
            decls,
            contains,
        } => {
            // Find the pre-created module scope and enter it.
            if let Some(mod_id) = st.find_module_scope(name) {
                let saved = st.enter_scope(mod_id);

                process_uses(st, uses, module_search_paths, layouts)?;
                process_implicit(st, implicit)?;
                process_decls(st, decls)?;
                process_contains(st, contains, module_search_paths, layouts)?;
                backfill_procedure_pointer_interfaces(st, mod_id);

                st.enter_scope(saved);
            }
        }
        ProgramUnit::Subroutine {
            name,
            args,
            prefix: _,
            bind: _,
            uses,
            imports: _,
            implicit,
            decls,
            body,
            contains,
        } => {
            let scope_id = st.push_scope(ScopeKind::Subroutine(name.clone()));
            // Store ordered arg names for VALUE lookup by callers.
            st.scope_mut(scope_id).arg_order = args
                .iter()
                .filter_map(|a| {
                    if let DummyArg::Name(n) = a {
                        Some(n.to_lowercase())
                    } else {
                        None
                    }
                })
                .collect();
            // Define dummy arguments as symbols.
            for arg in args {
                if let DummyArg::Name(arg_name) = arg {
                    st.define(Symbol {
                        name: arg_name.clone(),
                        kind: SymbolKind::Variable,
                        type_info: None,
                        attrs: SymbolAttrs::default(),
                        defined_at: unit.span,
                        scope: st.current_scope(),
                        arg_names: vec![],
                        const_value: None,
                    })?;
                }
            }
            process_uses(st, uses, module_search_paths, layouts)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            preload_stmt_uses(st, body, module_search_paths, layouts);
            process_contains(st, contains, module_search_paths, layouts)?;
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::Function {
            name,
            args,
            result,
            return_type,
            bind: _,
            prefix: _,
            uses,
            imports: _,
            implicit,
            decls,
            body,
            contains,
        } => {
            let host_scope = st.current_scope();
            let scope_id = st.push_scope(ScopeKind::Function(name.clone()));
            st.scope_mut(scope_id).arg_order = args
                .iter()
                .filter_map(|a| {
                    if let DummyArg::Name(n) = a {
                        Some(n.to_lowercase())
                    } else {
                        None
                    }
                })
                .collect();
            for arg in args {
                if let DummyArg::Name(arg_name) = arg {
                    st.define(Symbol {
                        name: arg_name.clone(),
                        kind: SymbolKind::Variable,
                        type_info: None,
                        attrs: SymbolAttrs::default(),
                        defined_at: unit.span,
                        scope: st.current_scope(),
                        arg_names: vec![],
                        const_value: None,
                    })?;
                }
            }
            // Define result variable.
            let result_name = result.as_deref().unwrap_or(name.as_str());
            st.define(Symbol {
                name: result_name.into(),
                kind: SymbolKind::Variable,
                type_info: return_type.as_ref().map(|ts| type_spec_to_info(ts, st)),
                attrs: SymbolAttrs::default(),
                defined_at: unit.span,
                scope: st.current_scope(),
                arg_names: vec![],
                const_value: None,
            })?;
            process_uses(st, uses, module_search_paths, layouts)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            backfill_function_result_type(
                st,
                host_scope,
                scope_id,
                name,
                result.as_deref().unwrap_or(name.as_str()),
            );
            preload_stmt_uses(st, body, module_search_paths, layouts);
            process_contains(st, contains, module_search_paths, layouts)?;
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::BlockData { name, uses, decls } => {
            let scope_name = name.clone().unwrap_or_else(|| "<block_data>".into());
            st.push_scope(ScopeKind::Program(scope_name));
            process_uses(st, uses, module_search_paths, layouts)?;
            process_decls(st, decls)?;
            st.pop_scope();
        }
        ProgramUnit::Submodule {
            parent,
            ancestor: _,
            name,
            uses,
            decls,
            contains,
        } => {
            // Find the parent module scope and inherit its symbols.
            // If the parent isn't compiled in this TU, load it from .amod.
            let parent_scope = st
                .find_module_scope(parent)
                .or_else(|| load_external_module(st, parent, module_search_paths, layouts));
            st.push_scope(ScopeKind::Submodule(name.clone()));
            // Import all parent module symbols into the submodule scope.
            // Per F2008 12.2.3.2: submodules see ALL parent entities,
            // including private ones — that's the whole point of the
            // submodule mechanism (host association).
            if let Some(pid) = parent_scope {
                let parent_syms: Vec<(String, String)> = st
                    .scope(pid)
                    .symbols
                    .iter()
                    .map(|(key, sym)| (sym.name.clone(), key.clone()))
                    .collect();
                for (sym_name, _key) in &parent_syms {
                    st.add_use_association(UseAssociation {
                        local_name: sym_name.clone(),
                        original_name: sym_name.clone(),
                        source_scope: pid,
                        is_submodule_access: true,
                    });
                }
            }
            process_uses(st, uses, module_search_paths, layouts)?;
            process_decls(st, decls)?;
            process_contains(st, contains, module_search_paths, layouts)?;
            let scope_id = st.current_scope();
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::InterfaceBlock {
            name,
            is_abstract: _,
            bodies,
        } => {
            // Collect each subprogram's name and return type BEFORE
            // pushing the Interface scope — the subprogram body gets
            // its own scope via resolve_unit, and we need to surface
            // the *declared* callable back into the enclosing scope
            // (otherwise IMPLICIT NONE rejects the call at the use
            // site, and generic dispatch can't see the body types).
            let mut outer_refs: Vec<InterfaceOuterRef> = Vec::new();
            for body in bodies {
                if let InterfaceBody::Subprogram(sub) = body {
                    match &sub.node {
                        ProgramUnit::Function {
                            name: fn_name,
                            return_type,
                            result,
                            decls,
                            args,
                            bind,
                            ..
                        } => {
                            let arg_names = args
                                .iter()
                                .filter_map(|a| {
                                    if let DummyArg::Name(n) = a {
                                        Some(n.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            let ti = return_type
                                .as_ref()
                                .map(|ts| type_spec_to_info(ts, st))
                                .or_else(|| {
                                    let result_name = result.as_deref().unwrap_or(fn_name.as_str());
                                    let key = result_name.to_lowercase();
                                    for d in decls {
                                        if let decl::Decl::TypeDecl {
                                            type_spec,
                                            entities,
                                            ..
                                        } = &d.node
                                        {
                                            for e in entities {
                                                if e.name.to_lowercase() == key {
                                                    return Some(type_spec_to_info(type_spec, st));
                                                }
                                            }
                                        }
                                    }
                                    None
                                });
                            outer_refs.push((
                                fn_name.clone(),
                                SymbolKind::Function,
                                ti,
                                arg_names,
                                normalized_bind_name(bind.as_ref(), fn_name),
                            ));
                        }
                        ProgramUnit::Subroutine {
                            name: fn_name,
                            args,
                            bind,
                            ..
                        } => {
                            let arg_names = args
                                .iter()
                                .filter_map(|a| {
                                    if let DummyArg::Name(n) = a {
                                        Some(n.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            outer_refs.push((
                                fn_name.clone(),
                                SymbolKind::Subroutine,
                                None,
                                arg_names,
                                normalized_bind_name(bind.as_ref(), fn_name),
                            ));
                        }
                        _ => {}
                    }
                }
            }

            st.push_scope(ScopeKind::Interface);
            let mut specific_names = Vec::new();
            for body in bodies {
                match body {
                    InterfaceBody::Subprogram(sub) => {
                        match &sub.node {
                            ProgramUnit::Function { name: fn_name, .. }
                            | ProgramUnit::Subroutine { name: fn_name, .. } => {
                                specific_names.push(fn_name.to_lowercase());
                            }
                            _ => {}
                        }
                        resolve_unit(st, sub, module_search_paths, layouts)?;
                    }
                    InterfaceBody::ModuleProcedure(names) => {
                        for n in names {
                            specific_names.push(n.to_lowercase());
                        }
                    }
                }
            }
            st.pop_scope();

            // Surface each declared procedure to the enclosing scope
            // so callers under IMPLICIT NONE can resolve the name,
            // and so BIND(C) external prototypes are callable.
            for (fn_name, kind, ti, arg_names, binding_label) in outer_refs {
                let span = unit.span;
                let _ = st.define(Symbol {
                    name: fn_name,
                    kind,
                    type_info: ti,
                    attrs: SymbolAttrs {
                        external: true,
                        binding_label,
                        ..Default::default()
                    },
                    defined_at: span,
                    scope: st.current_scope(),
                    arg_names,
                    const_value: None,
                });
            }

            // Register the generic interface name in the enclosing scope.
            if let Some(generic_name) = name {
                if !generic_name.is_empty() && !specific_names.is_empty() {
                    let merged_specifics = merged_visible_generic_specifics(
                        st,
                        st.current_scope(),
                        generic_name,
                        &specific_names,
                    );
                    let span = unit.span;
                    let define_result = st.define(Symbol {
                        name: generic_name.clone(),
                        kind: SymbolKind::NamedInterface,
                        type_info: None,
                        attrs: SymbolAttrs {
                            ..Default::default()
                        },
                        defined_at: span,
                        scope: st.current_scope(),
                        arg_names: merged_specifics.clone(),
                        const_value: None,
                    });
                    if define_result.is_err() {
                        let key = generic_name.to_ascii_lowercase();
                        if let Some(existing) =
                            st.scope_mut(st.current_scope()).symbols.get_mut(&key)
                        {
                            if existing.kind == SymbolKind::NamedInterface
                                || existing.kind == SymbolKind::DerivedType
                            {
                                merge_specific_names(&mut existing.arg_names, &merged_specifics);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn process_uses(
    st: &mut SymbolTable,
    uses: &[SpannedDecl],
    module_search_paths: &[std::path::PathBuf],
    type_layouts: &mut super::type_layout::TypeLayoutRegistry,
) -> Result<(), SemaError> {
    for use_decl in uses {
        if let Decl::UseStmt {
            module,
            nature: _,
            renames,
            only,
        } = &use_decl.node
        {
            // If the module isn't defined in-file, try loading from .amod.
            let mod_scope = st
                .find_module_scope(module)
                .or_else(|| load_external_module(st, module, module_search_paths, type_layouts));
            if let Some(mod_scope) = mod_scope {
                // Reject self-USE: a module cannot USE itself.
                if mod_scope == st.current_scope() {
                    return Err(SemaError {
                        msg: format!("module '{}' cannot USE itself", module),
                        span: use_decl.span,
                    });
                }
                if let Some(only_items) = only {
                    // USE ... ONLY: import specific names.
                    for item in only_items {
                        match item {
                            OnlyItem::Name(name) => {
                                st.add_use_association(UseAssociation {
                                    local_name: name.clone(),
                                    original_name: name.clone(),
                                    source_scope: mod_scope,
                                    is_submodule_access: false,
                                });
                            }
                            OnlyItem::Generic(name) => {
                                st.add_use_association(UseAssociation {
                                    local_name: name.clone(),
                                    original_name: name.clone(),
                                    source_scope: mod_scope,
                                    is_submodule_access: false,
                                });
                            }
                            OnlyItem::Rename(rename) => {
                                st.add_use_association(UseAssociation {
                                    local_name: rename.local.clone(),
                                    original_name: rename.remote.clone(),
                                    source_scope: mod_scope,
                                    is_submodule_access: false,
                                });
                            }
                        }
                    }
                } else {
                    // USE without ONLY: import all public symbols.
                    let mod_symbols: Vec<(String, String)> = st
                        .scope(mod_scope)
                        .symbols
                        .iter()
                        .filter(|(_, sym)| sym.attrs.access != Access::Private)
                        .map(|(key, sym)| (sym.name.clone(), key.clone()))
                        .collect();
                    for (name, _key) in &mod_symbols {
                        st.add_use_association(UseAssociation {
                            local_name: name.clone(),
                            original_name: name.clone(),
                            source_scope: mod_scope,
                            is_submodule_access: false,
                        });
                    }
                    // Apply renames.
                    for rename in renames {
                        st.add_use_association(UseAssociation {
                            local_name: rename.local.clone(),
                            original_name: rename.remote.clone(),
                            source_scope: mod_scope,
                            is_submodule_access: false,
                        });
                    }
                }
            } else {
                return Err(SemaError {
                    msg: format!("module '{}' not found (searched -I paths and current directory for {}.amod)", module, module.to_lowercase()),
                    span: use_decl.span,
                });
            }
        }
    }
    Ok(())
}

/// BLOCK constructs can carry their own USE statements inside a statement body.
/// We do not model block-local use associations in the symbol table yet, but we
/// still need the referenced modules loaded so later validation and lowering can
/// resolve imported procedures, derived types, and module globals.
fn ensure_uses_loaded(
    st: &mut SymbolTable,
    uses: &[SpannedDecl],
    module_search_paths: &[std::path::PathBuf],
    type_layouts: &mut super::type_layout::TypeLayoutRegistry,
) {
    for use_decl in uses {
        if let Decl::UseStmt { module, .. } = &use_decl.node {
            if st.find_module_scope(module).is_none() {
                let _ = load_external_module(st, module, module_search_paths, type_layouts);
            }
        }
    }
}

fn preload_stmt_uses(
    st: &mut SymbolTable,
    stmts: &[crate::ast::stmt::SpannedStmt],
    module_search_paths: &[std::path::PathBuf],
    type_layouts: &mut super::type_layout::TypeLayoutRegistry,
) {
    use crate::ast::stmt::Stmt;

    for stmt in stmts {
        match &stmt.node {
            Stmt::IfConstruct {
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                preload_stmt_uses(st, then_body, module_search_paths, type_layouts);
                for (_, body) in else_ifs {
                    preload_stmt_uses(st, body, module_search_paths, type_layouts);
                }
                if let Some(body) = else_body {
                    preload_stmt_uses(st, body, module_search_paths, type_layouts);
                }
            }
            Stmt::IfStmt { action, .. } => {
                preload_stmt_uses(
                    st,
                    std::slice::from_ref(action.as_ref()),
                    module_search_paths,
                    type_layouts,
                );
            }
            Stmt::DoLoop { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::DoConcurrent { body, .. }
            | Stmt::Associate { body, .. }
            | Stmt::ForallConstruct { body, .. }
            | Stmt::WhereConstruct { body, .. } => {
                preload_stmt_uses(st, body, module_search_paths, type_layouts);
            }
            Stmt::ForallStmt { stmt: inner, .. }
            | Stmt::WhereStmt { stmt: inner, .. }
            | Stmt::Labeled { stmt: inner, .. } => {
                preload_stmt_uses(
                    st,
                    std::slice::from_ref(inner.as_ref()),
                    module_search_paths,
                    type_layouts,
                );
            }
            Stmt::Block {
                uses, ifaces, body, ..
            } => {
                ensure_uses_loaded(st, uses, module_search_paths, type_layouts);
                for iface in ifaces {
                    let _ = resolve_unit(st, iface, module_search_paths, type_layouts);
                }
                preload_stmt_uses(st, body, module_search_paths, type_layouts);
            }
            Stmt::SelectCase { cases, .. } => {
                for case in cases {
                    preload_stmt_uses(st, &case.body, module_search_paths, type_layouts);
                }
            }
            Stmt::SelectType { guards, .. } => {
                for guard in guards {
                    match guard {
                        crate::ast::stmt::TypeGuard::TypeIs { body, .. }
                        | crate::ast::stmt::TypeGuard::ClassIs { body, .. }
                        | crate::ast::stmt::TypeGuard::ClassDefault { body } => {
                            preload_stmt_uses(st, body, module_search_paths, type_layouts);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Try to load a module interface from an .amod file on the search path.
/// Creates a synthetic module scope in the symbol table and returns its ID.
fn load_external_module(
    st: &mut SymbolTable,
    module_name: &str,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut super::type_layout::TypeLayoutRegistry,
) -> Option<ScopeId> {
    use crate::lexer::{Position, Span};
    use crate::sema::amod;

    let filename = format!("{}.amod", module_name.to_lowercase());

    // Search -I paths then CWD.
    let mut candidates: Vec<std::path::PathBuf> =
        search_paths.iter().map(|p| p.join(&filename)).collect();
    candidates.push(std::path::PathBuf::from(&filename));

    let amod_path = candidates.iter().find(|p| p.exists())?;

    let iface = match amod::read_amod(amod_path) {
        Ok(iface) => iface,
        Err(e) => {
            eprintln!("warning: {}", e);
            return None;
        }
    };

    let dummy_span = Span {
        file_id: 0,
        start: Position { line: 0, col: 0 },
        end: Position { line: 0, col: 0 },
    };

    // Create a synthetic module scope.
    let scope_id = st.push_scope(ScopeKind::Module(iface.module_name.clone()));

    // Recursively resolve `@uses` dependencies so transitive USE
    // chains see re-exported symbols. Each dep becomes a
    // UseAssociation on this scope, exactly like `use foo` inside a
    // real source module, which makes lookup_in_guarded walk into
    // the dep's symbols. Without this, `USE amod_middle` where
    // middle does `use amod_base` never sees amod_base's symbols.
    for dep in &iface.dependencies {
        let dep_scope = st
            .find_module_scope(dep)
            .or_else(|| load_external_module(st, dep, search_paths, type_layouts));
        if let Some(dep_scope) = dep_scope {
            st.enter_scope(scope_id);
            // Re-export every public symbol of the dep by name, like
            // a bare `use <dep>` in source. The transitive lookup in
            // SymbolTable::lookup_in_guarded handles onward chaining.
            for (name, sym) in st
                .scope(dep_scope)
                .symbols
                .iter()
                .map(|(n, s)| (n.clone(), s.clone()))
                .collect::<Vec<_>>()
            {
                if matches!(sym.attrs.access, Access::Private) {
                    continue;
                }
                st.add_use_association(crate::sema::symtab::UseAssociation {
                    local_name: name.clone(),
                    original_name: name,
                    source_scope: dep_scope,
                    is_submodule_access: false,
                });
            }
        }
    }

    // Populate variables and parameters.
    for var in &iface.variables {
        let kind = if var.is_parameter {
            SymbolKind::Parameter
        } else if var.proc_pointer {
            SymbolKind::ProcedurePointer
        } else {
            SymbolKind::Variable
        };
        let attrs = SymbolAttrs {
            access: Access::Public,
            allocatable: var.allocatable,
            save: var.save,
            pointer: var.pointer,
            target: var.target,
            parameter: var.is_parameter,
            external: var.proc_pointer,
            procedure_iface: if var.proc_pointer {
                match &var.type_info {
                    Some(TypeInfo::Derived(name)) => Some(name.clone()),
                    _ => None,
                }
            } else {
                None
            },
            ..Default::default()
        };
        let _ = st.define(Symbol {
            name: var.name.clone(),
            kind,
            type_info: var.type_info.clone(),
            attrs,
            defined_at: dummy_span,
            scope: scope_id,
            arg_names: vec![],
            const_value: var.const_value,
        });
    }

    // Populate procedures. Each proc is defined as a symbol in the
    // module scope AND given its own Function/Subroutine scope whose
    // symbols carry the argument type_info. The dedicated scope is
    // what `resolve_generic_call` walks to match argument types at
    // call sites — without it, cross-TU generic dispatch sees no
    // candidates and fails.
    for proc in &iface.procedures {
        let attrs = SymbolAttrs {
            access: proc.access,
            allocatable: proc.result_allocatable,
            pointer: proc.result_pointer,
            pure: proc.pure,
            elemental: proc.elemental,
            binding_label: proc.binding_label.clone(),
            ..Default::default()
        };
        let arg_names: Vec<String> = proc
            .args
            .iter()
            .filter(|a| !a.hidden)
            .map(|a| a.name.clone())
            .collect();
        let _ = st.define(Symbol {
            name: proc.name.clone(),
            kind: proc.kind.clone(),
            type_info: proc.return_type.clone(),
            attrs,
            defined_at: dummy_span,
            scope: scope_id,
            arg_names: arg_names.clone(),
            const_value: None,
        });
        // Synthesise a Function/Subroutine scope for this procedure
        // so arg types survive to generic dispatch.
        let proc_scope_kind = match &proc.kind {
            crate::sema::symtab::SymbolKind::Function => ScopeKind::Function(proc.name.clone()),
            crate::sema::symtab::SymbolKind::Subroutine => ScopeKind::Subroutine(proc.name.clone()),
            _ => continue,
        };
        let proc_scope = st.push_scope(proc_scope_kind);
        st.scope_mut(proc_scope).arg_order = arg_names.clone();
        for arg in &proc.args {
            if arg.hidden {
                continue;
            }
            let arg_attrs = SymbolAttrs {
                intent: arg.intent,
                optional: arg.optional,
                value: arg.value,
                allocatable: arg.allocatable,
                pointer: arg.pointer,
                ..Default::default()
            };
            let _ = st.define(Symbol {
                name: arg.name.clone(),
                kind: crate::sema::symtab::SymbolKind::Variable,
                type_info: arg.type_info.clone(),
                attrs: arg_attrs,
                defined_at: dummy_span,
                scope: proc_scope,
                arg_names: vec![],
                const_value: None,
            });
        }
        st.pop_scope();
    }
    backfill_procedure_pointer_interfaces(st, scope_id);

    // Register type layouts.
    for layout in &iface.types {
        type_layouts.insert(layout.clone());
        // Also add a DerivedType symbol.
        let attrs = SymbolAttrs {
            access: Access::Public,
            ..Default::default()
        };
        let _ = st.define(Symbol {
            name: layout.name.clone(),
            kind: SymbolKind::DerivedType,
            type_info: None,
            attrs,
            defined_at: dummy_span,
            scope: scope_id,
            arg_names: vec![],
            const_value: None,
        });
    }

    // Register named generic interfaces. The specifics list rides
    // in `arg_names` to match how intra-file INTERFACE blocks are
    // stored by process_decls — `resolve_generic_call` reads it
    // when dispatching a call through the generic name.
    for iface_def in &iface.interfaces {
        let attrs = SymbolAttrs {
            access: Access::Public,
            ..Default::default()
        };
        let define_result = st.define(Symbol {
            name: iface_def.name.clone(),
            kind: SymbolKind::NamedInterface,
            type_info: None,
            attrs,
            defined_at: dummy_span,
            scope: scope_id,
            arg_names: iface_def.specifics.clone(),
            const_value: None,
        });
        if define_result.is_err() {
            let key = iface_def.name.to_ascii_lowercase();
            if let Some(existing) = st.scope_mut(scope_id).symbols.get_mut(&key) {
                if existing.kind == SymbolKind::NamedInterface
                    || existing.kind == SymbolKind::DerivedType
                {
                    merge_specific_names(&mut existing.arg_names, &iface_def.specifics);
                }
            }
        }
    }

    st.pop_scope();

    // Track the loaded interface so resolve_file can return it.
    LOADED_EXTERNAL_MODULES.with(|cell| cell.borrow_mut().push(iface));

    Some(scope_id)
}

/// Walk all program units and compute layouts for derived types.
fn compute_all_layouts(
    units: &[SpannedUnit],
    st: &SymbolTable,
    layouts: &mut super::type_layout::TypeLayoutRegistry,
) {
    let inherited_params = HashMap::new();
    let mut visible_param_cache: HashMap<ScopeId, HashMap<String, i64>> = HashMap::new();
    let mut exported_param_cache: HashMap<ScopeId, HashMap<String, i64>> = HashMap::new();
    for unit in units {
        let scope_id = find_unit_scope(st, 0, &unit.node).unwrap_or(0);
        collect_derived_type_layouts(
            &unit.node,
            scope_id,
            st,
            layouts,
            &inherited_params,
            &mut visible_param_cache,
            &mut exported_param_cache,
        );
    }
}

fn scope_matches_unit(kind: &ScopeKind, unit: &ProgramUnit) -> bool {
    match (kind, unit) {
        (ScopeKind::Program(lhs), ProgramUnit::Program { name, .. }) => name
            .as_deref()
            .map(|rhs| lhs.eq_ignore_ascii_case(rhs))
            .unwrap_or(false),
        (ScopeKind::Module(lhs), ProgramUnit::Module { name, .. })
        | (ScopeKind::Submodule(lhs), ProgramUnit::Submodule { name, .. })
        | (ScopeKind::Subroutine(lhs), ProgramUnit::Subroutine { name, .. })
        | (ScopeKind::Function(lhs), ProgramUnit::Function { name, .. }) => {
            lhs.eq_ignore_ascii_case(name)
        }
        _ => false,
    }
}

fn find_unit_scope(st: &SymbolTable, parent_scope: ScopeId, unit: &ProgramUnit) -> Option<ScopeId> {
    st.all_scopes().iter().find_map(|scope| {
        if scope.parent != Some(parent_scope) {
            return None;
        }
        if scope_matches_unit(&scope.kind, unit) {
            Some(scope.id)
        } else {
            None
        }
    })
}

fn exported_const_int_params(
    st: &SymbolTable,
    scope_id: ScopeId,
    _visible_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
    exported_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
) -> HashMap<String, i64> {
    if let Some(cached) = exported_cache.get(&scope_id) {
        return cached.clone();
    }

    let scope = st.scope(scope_id);
    let mut out = HashMap::new();

    for (name, sym) in &scope.symbols {
        if sym.attrs.parameter && sym.attrs.access != Access::Private {
            if let Some(value) = sym.const_value {
                out.entry(name.clone()).or_insert(value);
            }
        }
    }

    for assoc in &scope.use_associations {
        if let Some(sym) = st
            .scope(assoc.source_scope)
            .symbols
            .get(&assoc.original_name)
        {
            if sym.attrs.parameter
                && (sym.attrs.access != Access::Private || assoc.is_submodule_access)
            {
                if let Some(value) = sym.const_value {
                    out.entry(assoc.local_name.clone()).or_insert(value);
                }
            }
        }
    }

    let mut seen_use_scopes = HashSet::new();
    for assoc in &scope.use_associations {
        if assoc.local_name != assoc.original_name {
            continue;
        }
        if !seen_use_scopes.insert(assoc.source_scope) {
            continue;
        }
        for (name, value) in
            exported_const_int_params(st, assoc.source_scope, _visible_cache, exported_cache)
        {
            out.entry(name).or_insert(value);
        }
    }

    exported_cache.insert(scope_id, out.clone());
    out
}

fn visible_const_int_params(
    st: &SymbolTable,
    scope_id: ScopeId,
    visible_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
    exported_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
) -> HashMap<String, i64> {
    if let Some(cached) = visible_cache.get(&scope_id) {
        return cached.clone();
    }

    let scope = st.scope(scope_id);
    let mut out = HashMap::new();

    for (name, sym) in &scope.symbols {
        if sym.attrs.parameter {
            if let Some(value) = sym.const_value {
                out.entry(name.clone()).or_insert(value);
            }
        }
    }

    for assoc in &scope.use_associations {
        if out.contains_key(&assoc.local_name) {
            continue;
        }
        if let Some(sym) = st
            .scope(assoc.source_scope)
            .symbols
            .get(&assoc.original_name)
        {
            if sym.attrs.parameter
                && (sym.attrs.access != Access::Private || assoc.is_submodule_access)
            {
                if let Some(value) = sym.const_value {
                    out.insert(assoc.local_name.clone(), value);
                }
            }
        }
    }

    let mut seen_use_scopes = HashSet::new();
    for assoc in &scope.use_associations {
        if assoc.local_name != assoc.original_name {
            continue;
        }
        if !seen_use_scopes.insert(assoc.source_scope) {
            continue;
        }
        for (name, value) in
            exported_const_int_params(st, assoc.source_scope, visible_cache, exported_cache)
        {
            out.entry(name).or_insert(value);
        }
    }

    if let Some(parent) = scope.parent {
        if st.scope(parent).kind != ScopeKind::Global {
            for (name, value) in visible_const_int_params(st, parent, visible_cache, exported_cache)
            {
                out.entry(name).or_insert(value);
            }
        }
    }

    visible_cache.insert(scope_id, out.clone());
    out
}

fn collect_derived_type_layouts(
    unit: &ProgramUnit,
    scope_id: ScopeId,
    st: &SymbolTable,
    layouts: &mut super::type_layout::TypeLayoutRegistry,
    inherited_params: &HashMap<String, i64>,
    visible_param_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
    exported_param_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
) {
    let (decls, contains) = match unit {
        ProgramUnit::Program {
            decls, contains, ..
        }
        | ProgramUnit::Module {
            decls, contains, ..
        }
        | ProgramUnit::Subroutine {
            decls, contains, ..
        }
        | ProgramUnit::Function {
            decls, contains, ..
        } => (decls, contains),
        _ => return,
    };
    let mut seed_params = inherited_params.clone();
    seed_params.extend(visible_const_int_params(
        st,
        scope_id,
        visible_param_cache,
        exported_param_cache,
    ));
    let host_module = match &st.scope(scope_id).kind {
        crate::sema::symtab::ScopeKind::Module(module_name)
        | crate::sema::symtab::ScopeKind::Submodule(module_name) => Some(module_name.as_str()),
        _ => None,
    };
    let const_params = collect_const_int_params(decls, &seed_params);
    let empty_derived_field_inits = HashMap::new();
    register_local_type_layouts(
        decls,
        host_module,
        layouts,
        &const_params,
        &empty_derived_field_inits,
    );
    let const_derived_field_inits =
        collect_const_derived_field_inits(decls, layouts, &const_params);
    register_local_type_layouts(
        decls,
        host_module,
        layouts,
        &const_params,
        &const_derived_field_inits,
    );
    for sub in contains {
        let sub_scope_id = find_unit_scope(st, scope_id, &sub.node).unwrap_or(scope_id);
        collect_derived_type_layouts(
            &sub.node,
            sub_scope_id,
            st,
            layouts,
            &const_params,
            visible_param_cache,
            exported_param_cache,
        );
    }
}

fn parse_boz_i64(text: &str, base: crate::ast::expr::BozBase) -> Option<i64> {
    let radix = match base {
        crate::ast::expr::BozBase::Binary => 2,
        crate::ast::expr::BozBase::Octal => 8,
        crate::ast::expr::BozBase::Hex => 16,
    };
    let digits: String = text
        .chars()
        .skip_while(|c| !matches!(c, '\'' | '"'))
        .skip(1)
        .take_while(|c| !matches!(c, '\'' | '"'))
        .collect();
    i64::from_str_radix(&digits, radix).ok()
}

fn eval_const_int_expr_with_params(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
) -> Option<i64> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => {
            let clean = text.split('_').next().unwrap_or(text);
            clean.parse::<i64>().ok()
        }
        Expr::BozLiteral { text, base } => parse_boz_i64(text, *base),
        Expr::Name { name } => const_params.get(&name.to_lowercase()).copied(),
        Expr::UnaryOp { op, operand } => {
            let v = eval_const_int_expr_with_params(operand, const_params)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-v),
                crate::ast::expr::UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_const_int_expr_with_params(left, const_params)?;
            let r = eval_const_int_expr_with_params(right, const_params)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(l + r),
                crate::ast::expr::BinaryOp::Sub => Some(l - r),
                crate::ast::expr::BinaryOp::Mul => Some(l * r),
                crate::ast::expr::BinaryOp::Div if r != 0 => Some(l / r),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => eval_const_int_expr_with_params(inner, const_params),
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let first_arg_val = args.first().and_then(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    eval_const_int_expr_with_params(e, const_params)
                } else {
                    None
                }
            });
            match name.to_lowercase().as_str() {
                "selected_int_kind" => {
                    let r = first_arg_val?;
                    Some(if r <= 2 {
                        1
                    } else if r <= 4 {
                        2
                    } else if r <= 9 {
                        4
                    } else if r <= 18 {
                        8
                    } else if r <= 38 {
                        16
                    } else {
                        -1
                    })
                }
                "selected_real_kind" => {
                    let p = first_arg_val?;
                    Some(if p <= 6 {
                        4
                    } else if p <= 15 {
                        8
                    } else {
                        -1
                    })
                }
                "kind" => {
                    let arg = args.first()?;
                    let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                        return None;
                    };
                    match &e.node {
                        Expr::RealLiteral { text, .. } => {
                            Some(if text.contains('d') || text.contains('D') {
                                8
                            } else {
                                4
                            })
                        }
                        Expr::IntegerLiteral { .. } => Some(4),
                        _ => None,
                    }
                }
                "int" => {
                    let arg = args.first()?;
                    let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                        return None;
                    };
                    match &e.node {
                        Expr::RealLiteral { text, .. } => text
                            .replace('d', "e")
                            .replace('D', "E")
                            .split('_')
                            .next()
                            .unwrap_or(text)
                            .parse::<f64>()
                            .ok()
                            .map(|v| v.trunc() as i64),
                        _ => eval_const_int_expr_with_params(e, const_params),
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn collect_const_int_params(
    decls: &[SpannedDecl],
    inherited_params: &HashMap<String, i64>,
) -> HashMap<String, i64> {
    let mut params = inherited_params.clone();
    for decl in decls {
        let Decl::TypeDecl {
            attrs, entities, ..
        } = &decl.node
        else {
            continue;
        };
        if !attrs.iter().any(|a| matches!(a, Attribute::Parameter)) {
            continue;
        }
        for entity in entities {
            params.remove(&entity.name.to_lowercase());
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for decl in decls {
            let Decl::TypeDecl {
                attrs, entities, ..
            } = &decl.node
            else {
                continue;
            };
            if !attrs.iter().any(|a| matches!(a, Attribute::Parameter)) {
                continue;
            }
            for entity in entities {
                let key = entity.name.to_lowercase();
                if params.contains_key(&key) {
                    continue;
                }
                let Some(init) = entity.init.as_ref() else {
                    continue;
                };
                if let Some(value) = eval_const_int_expr_with_params(init, &params) {
                    params.insert(key, value);
                    changed = true;
                }
            }
        }
    }

    params
}

fn collect_const_derived_field_inits(
    decls: &[SpannedDecl],
    layouts: &super::type_layout::TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
) -> HashMap<String, super::type_layout::FieldDefaultInit> {
    use super::type_layout::{
        derived_param_field_lookup_key, eval_const_field_default_init_for_layout, FieldDefaultInit,
    };

    let mut field_inits = HashMap::new();

    let mut changed = true;
    while changed {
        changed = false;
        for decl in decls {
            let Decl::TypeDecl {
                type_spec,
                attrs,
                entities,
            } = &decl.node
            else {
                continue;
            };
            if !attrs.iter().any(|a| matches!(a, Attribute::Parameter)) {
                continue;
            }

            let type_name = match type_spec {
                TypeSpec::Type(name) | TypeSpec::Class(name) => name.as_str(),
                _ => continue,
            };
            let Some(layout) = layouts.get(type_name) else {
                continue;
            };

            for entity in entities {
                let Some(init_expr) = entity.init.as_ref() else {
                    continue;
                };
                let Some(FieldDefaultInit::Derived(overrides)) =
                    eval_const_field_default_init_for_layout(
                        &TypeInfo::Derived(type_name.to_string()),
                        init_expr,
                        layouts,
                        const_params,
                        &field_inits,
                    )
                else {
                    continue;
                };

                let mut combined = HashMap::new();
                for field in &layout.fields {
                    if let Some(default_init) = &field.default_init {
                        combined.insert(
                            field.name.to_ascii_lowercase(),
                            (field.name.clone(), default_init.clone()),
                        );
                    }
                }
                for (field_name, field_init) in overrides {
                    combined.insert(
                        field_name.to_ascii_lowercase(),
                        (field_name, field_init),
                    );
                }

                for (_field_key, (field_name, field_init)) in combined {
                    let key = derived_param_field_lookup_key(&entity.name, &field_name);
                    let should_update = field_inits
                        .get(&key)
                        .map(|existing| existing != &field_init)
                        .unwrap_or(true);
                    if should_update {
                        field_inits.insert(key, field_init);
                        changed = true;
                    }
                }
            }
        }
    }

    field_inits
}

fn register_local_type_layouts(
    decls: &[SpannedDecl],
    host_module: Option<&str>,
    layouts: &mut super::type_layout::TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    const_derived_field_inits: &HashMap<String, super::type_layout::FieldDefaultInit>,
) {
    for decl in decls {
        if let Decl::DerivedTypeDef {
            name,
            extends,
            attrs,
            components,
            type_bound_procs,
            final_procs,
            ..
        } = &decl.node
        {
            let parent = extends.as_ref().and_then(|p| layouts.get(p)).cloned();
            let is_abstract = attrs
                .iter()
                .any(|attr| matches!(attr, crate::ast::decl::TypeAttr::Abstract));
            let layout = super::type_layout::compute_layout_with_attrs(
                name,
                host_module,
                type_bound_procs,
                final_procs,
                components,
                parent.as_ref(),
                is_abstract,
                layouts,
                const_params,
                const_derived_field_inits,
            );
            // Don't overwrite a layout that has bound_procs or final_procs with one that doesn't.
            // This handles the case where a subroutine redefines a type without CONTAINS.
            let dominated = layouts
                .get(&name.to_lowercase())
                .map(|existing| {
                    let existing_has =
                        !existing.bound_procs.is_empty() || !existing.final_procs.is_empty();
                    let new_has = !layout.bound_procs.is_empty() || !layout.final_procs.is_empty();
                    existing_has && !new_has
                })
                .unwrap_or(false);
            if !dominated {
                layouts.insert(layout);
            }
        }
    }
}

fn process_implicit(st: &mut SymbolTable, implicit_stmts: &[SpannedDecl]) -> Result<(), SemaError> {
    for stmt in implicit_stmts {
        match &stmt.node {
            Decl::ImplicitNone { type_, external } => {
                st.set_implicit_none(*type_, *external);
            }
            Decl::ImplicitStmt { specs } => {
                for spec in specs {
                    let itype = match &spec.type_spec {
                        TypeSpec::Integer(_) => ImplicitType::Integer,
                        TypeSpec::Real(_) => ImplicitType::Real,
                        TypeSpec::DoublePrecision => ImplicitType::DoublePrecision,
                        TypeSpec::Complex(_) => ImplicitType::Complex,
                        TypeSpec::Logical(_) => ImplicitType::Logical,
                        TypeSpec::Character(_) => ImplicitType::Character,
                        _ => continue,
                    };
                    for (start, end) in &spec.ranges {
                        st.set_implicit_rule(*start, *end, itype);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn process_decls(st: &mut SymbolTable, decls: &[SpannedDecl]) -> Result<(), SemaError> {
    // Collect AccessList entries — they must be applied AFTER all TypeDecls
    // because the list may reference symbols declared later in the module.
    let mut pending_access: Vec<(Access, Vec<String>)> = Vec::new();
    for decl in decls {
        match &decl.node {
            Decl::AccessDefault { access } => match access {
                Attribute::Private => st.set_default_access(Access::Private),
                Attribute::Public => st.set_default_access(Access::Public),
                _ => {}
            },
            Decl::AccessList { access, names } => {
                let acc = match access {
                    Attribute::Private => Access::Private,
                    Attribute::Public => Access::Public,
                    _ => continue,
                };
                pending_access.push((acc, names.clone()));
            }
            Decl::TypeDecl {
                type_spec,
                attrs,
                entities,
            } => {
                let mut type_info = type_spec_to_info(type_spec, st);
                let mut sym_attrs =
                    attrs_to_symbol_attrs(attrs, st.default_access(st.current_scope()));
                let mut kind = if sym_attrs.parameter {
                    SymbolKind::Parameter
                } else {
                    SymbolKind::Variable
                };
                let mut arg_names = Vec::new();

                if sym_attrs.external {
                    if let TypeSpec::Type(iface_name) = type_spec {
                        sym_attrs.procedure_iface = Some(iface_name.clone());
                        if sym_attrs.pointer {
                            kind = SymbolKind::ProcedurePointer;
                        }
                        if let Some(iface_sym) =
                            st.find_symbol_any_scope(&iface_name.to_lowercase())
                        {
                            type_info = iface_sym
                                .type_info
                                .clone()
                                .unwrap_or_else(|| type_info.clone());
                            arg_names = iface_sym.arg_names.clone();
                        }
                    }
                }

                for entity in entities {
                    let key = entity.name.to_lowercase();
                    if st.scope(st.current_scope()).symbols.contains_key(&key) {
                        // Symbol already exists (e.g., dummy argument) — update type info.
                        let sym = st
                            .scope_mut(st.current_scope())
                            .symbols
                            .get_mut(&key)
                            .unwrap();
                        sym.kind = kind.clone();
                        sym.type_info = Some(type_info.clone());
                        sym.attrs = sym_attrs.clone();
                        sym.arg_names = arg_names.clone();
                    } else {
                        // Try to fold PARAMETER initializers to a
                        // compile-time integer so .amod can carry the
                        // value and consumers can inline it.
                        let const_value = if sym_attrs.parameter {
                            entity
                                .init
                                .as_ref()
                                .and_then(|e| eval_const_int_expr(e, st))
                        } else {
                            None
                        };
                        st.define(Symbol {
                            name: entity.name.clone(),
                            kind: kind.clone(),
                            type_info: Some(type_info.clone()),
                            attrs: sym_attrs.clone(),
                            defined_at: decl.span,
                            scope: st.current_scope(),
                            arg_names: arg_names.clone(),
                            const_value,
                        })?;
                    }
                }
            }
            Decl::DerivedTypeDef { name, attrs, .. } => {
                let mut sym_attrs = SymbolAttrs {
                    access: st.default_access(st.current_scope()),
                    ..SymbolAttrs::default()
                };
                for attr in attrs {
                    match attr {
                        decl::TypeAttr::Public => sym_attrs.access = Access::Public,
                        decl::TypeAttr::Private => sym_attrs.access = Access::Private,
                        _ => {}
                    }
                }
                st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::DerivedType,
                    type_info: None,
                    attrs: sym_attrs,
                    defined_at: decl.span,
                    scope: st.current_scope(),
                    arg_names: vec![],
                    const_value: None,
                })?;
            }
            Decl::EnumDef { enumerators } => {
                let mut next_value = 0i64;
                for (name, value_expr) in enumerators {
                    let const_value = if let Some(expr) = value_expr {
                        eval_const_int_expr(expr, st).unwrap_or(next_value)
                    } else {
                        next_value
                    };
                    next_value = const_value + 1;
                    st.define(Symbol {
                        name: name.clone(),
                        kind: SymbolKind::Parameter,
                        type_info: Some(TypeInfo::Integer { kind: None }),
                        attrs: SymbolAttrs {
                            access: st.default_access(st.current_scope()),
                            parameter: true,
                            ..Default::default()
                        },
                        defined_at: decl.span,
                        scope: st.current_scope(),
                        arg_names: vec![],
                        const_value: Some(const_value),
                    })?;
                }
            }
            _ => {}
        }
    }
    // Apply deferred access-list overrides after all symbols are declared.
    for (access, names) in &pending_access {
        for name in names {
            st.set_symbol_access(name, *access);
        }
    }
    Ok(())
}

fn process_contains(
    st: &mut SymbolTable,
    contains: &[SpannedUnit],
    module_search_paths: &[std::path::PathBuf],
    layouts: &mut super::type_layout::TypeLayoutRegistry,
) -> Result<(), SemaError> {
    for unit in contains {
        // Register the subprogram name in the current scope before descending.
        match &unit.node {
            ProgramUnit::Subroutine {
                name, prefix, bind, ..
            } => {
                let elemental = prefix
                    .iter()
                    .any(|p| matches!(p, crate::ast::unit::Prefix::Elemental));
                let pure = elemental
                    || prefix
                        .iter()
                        .any(|p| matches!(p, crate::ast::unit::Prefix::Pure));
                let attrs = SymbolAttrs {
                    pure,
                    elemental,
                    binding_label: normalized_bind_name(bind.as_ref(), name),
                    ..Default::default()
                };
                let _ignore_dup = st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Subroutine,
                    type_info: None,
                    attrs,
                    defined_at: unit.span,
                    scope: st.current_scope(),
                    arg_names: vec![],
                    const_value: None,
                });
            }
            ProgramUnit::Function {
                name,
                return_type,
                result,
                decls,
                prefix,
                bind,
                ..
            } => {
                let ret_type_info = return_type
                    .as_ref()
                    .map(|ts| type_spec_to_info(ts, st))
                    .or_else(|| {
                        // Infer return type from result variable's declaration.
                        let result_name = result.as_deref().unwrap_or(name.as_str());
                        let key = result_name.to_lowercase();
                        for d in decls {
                            if let decl::Decl::TypeDecl {
                                type_spec,
                                entities,
                                ..
                            } = &d.node
                            {
                                for e in entities {
                                    if e.name.to_lowercase() == key {
                                        return Some(type_spec_to_info(type_spec, st));
                                    }
                                }
                            }
                        }
                        None
                    });
                let fn_elemental = prefix
                    .iter()
                    .any(|p| matches!(p, crate::ast::unit::Prefix::Elemental));
                let fn_pure = fn_elemental
                    || prefix
                        .iter()
                        .any(|p| matches!(p, crate::ast::unit::Prefix::Pure));
                let result_attrs = function_result_attrs(name, result, decls);
                let fn_attrs = SymbolAttrs {
                    allocatable: result_attrs.allocatable,
                    pointer: result_attrs.pointer,
                    pure: fn_pure,
                    elemental: fn_elemental,
                    binding_label: normalized_bind_name(bind.as_ref(), name),
                    ..Default::default()
                };
                let _ignore_dup = st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    type_info: ret_type_info,
                    attrs: fn_attrs,
                    defined_at: unit.span,
                    scope: st.current_scope(),
                    arg_names: vec![],
                    const_value: None,
                });
            }
            _ => {}
        }
        resolve_unit(st, unit, module_search_paths, layouts)?;
    }
    Ok(())
}

// ---- Helpers ----

/// Extract a compile-time integer kind value from a KindSelector.
/// Try to evaluate a PARAMETER initializer to a compile-time i64.
/// Handles integer literals, negation, binary ops, parenthesized
/// expressions, and Name references that resolve to PARAMETERs
/// with known const_value in the current scope chain.
fn eval_const_int_expr(expr: &crate::ast::expr::SpannedExpr, st: &SymbolTable) -> Option<i64> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => {
            let clean = text.split('_').next().unwrap_or(text);
            clean.parse::<i64>().ok()
        }
        Expr::BozLiteral { text, base } => parse_boz_i64(text, *base),
        Expr::Name { name } => {
            // Look up the name in the current scope chain.
            let sym = st.lookup_in(st.current_scope(), &name.to_lowercase())?;
            if sym.attrs.parameter {
                sym.const_value
            } else {
                None
            }
        }
        Expr::UnaryOp { op, operand } => {
            let v = eval_const_int_expr(operand, st)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-v),
                crate::ast::expr::UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_const_int_expr(left, st)?;
            let r = eval_const_int_expr(right, st)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(l + r),
                crate::ast::expr::BinaryOp::Sub => Some(l - r),
                crate::ast::expr::BinaryOp::Mul => Some(l * r),
                crate::ast::expr::BinaryOp::Div if r != 0 => Some(l / r),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => eval_const_int_expr(inner, st),
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                let first_arg_val = args.first().and_then(|a| {
                    if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                        eval_const_int_expr(e, st)
                    } else {
                        None
                    }
                });
                match key.as_str() {
                    "selected_int_kind" => {
                        let r = first_arg_val?;
                        Some(if r <= 2 {
                            1
                        } else if r <= 4 {
                            2
                        } else if r <= 9 {
                            4
                        } else if r <= 18 {
                            8
                        } else if r <= 38 {
                            16
                        } else {
                            -1
                        })
                    }
                    "selected_real_kind" => {
                        let p = first_arg_val?;
                        Some(if p <= 6 {
                            4
                        } else if p <= 15 {
                            8
                        } else {
                            -1
                        })
                    }
                    "kind" => {
                        if let Some(arg) = args.first() {
                            if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                                match &e.node {
                                    Expr::RealLiteral { text, .. } => {
                                        Some(if text.contains('d') || text.contains('D') {
                                            8
                                        } else {
                                            4
                                        })
                                    }
                                    Expr::IntegerLiteral { .. } => Some(4),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    "int" => {
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        match &e.node {
                            Expr::RealLiteral { text, .. } => text
                                .replace('d', "e")
                                .replace('D', "E")
                                .split('_')
                                .next()
                                .unwrap_or(text)
                                .parse::<f64>()
                                .ok()
                                .map(|v| v.trunc() as i64),
                            _ => eval_const_int_expr(e, st),
                        }
                    }
                    "range" => {
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        let ty = match &e.node {
                            Expr::Name { name } => st
                                .lookup_in(st.current_scope(), &name.to_lowercase())
                                .and_then(|sym| sym.type_info.as_ref()),
                            Expr::ParenExpr { inner } => match &inner.node {
                                Expr::Name { name } => st
                                    .lookup_in(st.current_scope(), &name.to_lowercase())
                                    .and_then(|sym| sym.type_info.as_ref()),
                                _ => None,
                            },
                            _ => None,
                        }?;
                        match ty {
                            TypeInfo::Integer { kind } => Some(match kind
                                .unwrap_or(crate::driver::defaults::default_int_kind())
                            {
                                1 => 2,
                                2 => 4,
                                4 => 9,
                                8 => 18,
                                16 => 38,
                                _ => return None,
                            }),
                            TypeInfo::Real { kind } => Some(match kind
                                .unwrap_or(crate::driver::defaults::default_real_kind())
                            {
                                4 => 37,
                                8 => 307,
                                _ => return None,
                            }),
                            TypeInfo::DoublePrecision => Some(307),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn extract_kind(sel: &Option<decl::KindSelector>, st: &SymbolTable) -> Option<u8> {
    use crate::ast::expr::Expr;
    match sel {
        Some(decl::KindSelector::Expr(e)) | Some(decl::KindSelector::Star(e)) => {
            match &e.node {
                Expr::IntegerLiteral { text, .. } => text.parse().ok(),
                Expr::Name { name } => {
                    // Resolve named constant (e.g., c_double, real64, int64).
                    let key = name.to_lowercase();
                    st.lookup_in(st.current_scope(), &key)
                        .and_then(|sym| sym.const_value.map(|v| v as u8))
                }
                _ => None,
            }
        }
        None => None,
    }
}

/// Extract character length from a CharSelector.
fn extract_char_len(sel: &Option<decl::CharSelector>, st: &SymbolTable) -> Option<i64> {
    match sel {
        Some(cs) => match &cs.len {
            Some(decl::LenSpec::Expr(e)) => eval_const_int_expr(e, st),
            Some(decl::LenSpec::Star) => None,  // assumed length
            Some(decl::LenSpec::Colon) => None, // deferred length
            None => None,
        },
        None => None,
    }
}

fn type_spec_to_info(ts: &TypeSpec, st: &SymbolTable) -> TypeInfo {
    match ts {
        TypeSpec::Integer(sel) => TypeInfo::Integer {
            kind: extract_kind(sel, st),
        },
        TypeSpec::Real(sel) => TypeInfo::Real {
            kind: extract_kind(sel, st),
        },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(sel) => TypeInfo::Complex {
            kind: extract_kind(sel, st),
        },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(sel) => TypeInfo::Logical {
            kind: extract_kind(sel, st),
        },
        TypeSpec::Character(sel) => TypeInfo::Character {
            len: extract_char_len(sel, st),
            kind: None,
        },
        TypeSpec::Type(name) => TypeInfo::Derived(name.clone()),
        TypeSpec::Class(name) => TypeInfo::Class(name.clone()),
        TypeSpec::ClassStar => TypeInfo::ClassStar,
        TypeSpec::TypeStar => TypeInfo::TypeStar,
    }
}

fn attrs_to_symbol_attrs(attrs: &[Attribute], default_access: Access) -> SymbolAttrs {
    let mut sa = SymbolAttrs {
        access: default_access,
        ..SymbolAttrs::default()
    };
    for attr in attrs {
        match attr {
            Attribute::Allocatable => sa.allocatable = true,
            Attribute::Pointer => sa.pointer = true,
            Attribute::Target => sa.target = true,
            Attribute::Optional => sa.optional = true,
            Attribute::Save => sa.save = true,
            Attribute::Parameter => sa.parameter = true,
            Attribute::Value => sa.value = true,
            Attribute::External => sa.external = true,
            Attribute::Intrinsic => sa.intrinsic = true,
            Attribute::Public => sa.access = Access::Public,
            Attribute::Private => sa.access = Access::Private,
            Attribute::Intent(intent) => {
                sa.intent = Some(match intent {
                    decl::Intent::In => Intent::In,
                    decl::Intent::Out => Intent::Out,
                    decl::Intent::InOut => Intent::InOut,
                });
            }
            _ => {}
        }
    }
    sa
}

fn function_result_attrs(
    function_name: &str,
    result: &Option<String>,
    decls: &[crate::ast::decl::SpannedDecl],
) -> SymbolAttrs {
    let result_key = result
        .as_deref()
        .unwrap_or(function_name)
        .to_ascii_lowercase();
    for decl in decls {
        let crate::ast::decl::Decl::TypeDecl {
            attrs, entities, ..
        } = &decl.node
        else {
            continue;
        };
        if entities
            .iter()
            .any(|entity| entity.name.eq_ignore_ascii_case(&result_key))
        {
            return attrs_to_symbol_attrs(attrs, Access::Default);
        }
    }
    SymbolAttrs::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn resolve_source(src: &str) -> SymbolTable {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        resolve_file(&units, &[]).unwrap().st
    }

    fn resolve_source_with_layouts(src: &str) -> ResolveResult {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        resolve_file(&units, &[]).unwrap()
    }

    // ---- Integration tests ----

    #[test]
    fn simple_program_declarations() {
        let st = resolve_source(
            "program test\n  implicit none\n  integer :: x, y\n  real :: z\nend program\n",
        );
        // Should have x, y, z defined.
        // Navigate to the program scope.
        let prog_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Program(_)))
            .unwrap();
        assert!(prog_scope.symbols.contains_key("x"));
        assert!(prog_scope.symbols.contains_key("y"));
        assert!(prog_scope.symbols.contains_key("z"));
    }

    #[test]
    fn implicit_none_enforced() {
        let st = resolve_source("program test\n  implicit none\n  integer :: x\nend program\n");
        let prog_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Program(_)))
            .unwrap();
        assert!(prog_scope.implicit_rules.none_type);
    }

    #[test]
    fn module_use_association() {
        let st = resolve_source(
            "\
module mymod
  implicit none
  integer :: shared_var
end module

program main
  use mymod
  implicit none
end program
",
        );
        // shared_var should be in the module scope.
        let mod_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Module(ref n) if n == "mymod"))
            .unwrap();
        assert!(mod_scope.symbols.contains_key("shared_var"));

        // The program should have a USE association for shared_var.
        let prog_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Program(_)))
            .unwrap();
        assert!(!prog_scope.use_associations.is_empty());
    }

    #[test]
    fn subroutine_with_args() {
        let st = resolve_source("subroutine foo(x, y)\n  real :: x, y\nend subroutine\n");
        let sub_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Subroutine(ref n) if n == "foo"))
            .unwrap();
        assert!(sub_scope.symbols.contains_key("x"));
        assert!(sub_scope.symbols.contains_key("y"));
    }

    #[test]
    fn function_result_variable() {
        let st = resolve_source(
            "function square(x) result(y)\n  real :: x, y\n  y = x * x\nend function\n",
        );
        let fn_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Function(ref n) if n == "square"))
            .unwrap();
        assert!(fn_scope.symbols.contains_key("x"));
        assert!(fn_scope.symbols.contains_key("y"));
    }

    #[test]
    fn contains_creates_child_scope() {
        let st = resolve_source(
            "\
program main
  implicit none
  integer :: x
contains
  subroutine inner()
    integer :: local_var
  end subroutine
end program
",
        );
        // inner should be its own scope.
        let inner_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Subroutine(ref n) if n == "inner"))
            .unwrap();
        assert!(inner_scope.symbols.contains_key("local_var"));

        // inner should be registered as a symbol in the program scope.
        let prog_scope = st
            .scopes
            .iter()
            .find(|s| matches!(s.kind, ScopeKind::Program(_)))
            .unwrap();
        assert!(prog_scope.symbols.contains_key("inner"));
    }

    #[test]
    fn derived_type_defined() {
        let st = resolve_source(
            "module m\n  type :: mytype\n    integer :: field\n  end type\nend module\n",
        );
        let mod_scope = st
            .scopes
            .iter()
            .find(|s| matches!(&s.kind, ScopeKind::Module(n) if n == "m"))
            .unwrap();
        assert!(mod_scope.symbols.contains_key("mytype"));
        assert_eq!(mod_scope.symbols["mytype"].kind, SymbolKind::DerivedType);
    }

    #[test]
    fn imported_named_character_params_feed_type_layouts() {
        let resolved = resolve_source_with_layouts(
            "module cfg\n  implicit none\n  integer, parameter :: max_token_len = 16\nend module\n\nmodule m\n  use cfg, only: max_token_len\n  implicit none\n  type :: token_t\n    character(len=max_token_len), allocatable :: value(:)\n    character(len=max_token_len) :: tag = ''\n  end type\nend module\n",
        );
        let layout = resolved
            .type_layouts
            .get("token_t")
            .expect("missing token_t layout");
        let value = layout.field("value").expect("missing value field");
        let tag = layout.field("tag").expect("missing tag field");

        assert!(matches!(
            value.type_info,
            TypeInfo::Character {
                len: Some(16),
                kind: None
            }
        ));
        assert!(matches!(
            tag.type_info,
            TypeInfo::Character {
                len: Some(16),
                kind: None
            }
        ));
    }
}
