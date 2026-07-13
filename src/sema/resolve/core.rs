//! Symbol resolution — walks the AST and populates symbol tables.
//!
//! First pass: collect declarations, create scopes, process USE/IMPLICIT.
//! This establishes the symbol table that type checking (Sprint 13) will use.

use crate::ast::decl;
use crate::ast::decl::{Attribute, Decl, SpannedDecl, TypeSpec};
use crate::ast::stmt::{SpannedStmt, Stmt};
use crate::ast::unit::*;
use crate::sema::symtab::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::statement_functions::detect_statement_functions;
use super::type_resolution::{derived_char_init_len, entity_char_len_to_info, type_spec_to_info};
use super::use_resolution::{load_external_module, preload_stmt_uses, process_uses};

thread_local! {
    /// Track externally loaded module interfaces so resolve_file can
    /// return them to the driver for globals extraction.
    pub(super) static LOADED_EXTERNAL_MODULES: RefCell<Vec<crate::sema::amod::ModuleInterface>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn merge_specific_names(into: &mut Vec<String>, additional: &[String]) {
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
    pub type_layouts: crate::sema::type_layout::TypeLayoutRegistry,
    pub external_modules: Vec<crate::sema::amod::ModuleInterface>,
}

pub fn resolve_file(
    units: &[SpannedUnit],
    module_search_paths: &[std::path::PathBuf],
    target_layout: crate::target::TargetLayout,
) -> Result<ResolveResult, SemaError> {
    let mut st = SymbolTable::new();
    let mut layouts = crate::sema::type_layout::TypeLayoutRegistry::new();

    // Register intrinsic modules (iso_c_binding, iso_fortran_env) so USE can find them.
    crate::sema::intrinsic_modules::register_intrinsic_modules(&mut st);

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
    compute_all_layouts(target_layout, units, &st, &mut layouts);

    Ok(ResolveResult {
        st,
        type_layouts: layouts,
        external_modules,
    })
}

pub(super) fn backfill_procedure_pointer_interfaces(st: &mut SymbolTable, scope_id: ScopeId) {
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
        Some(sym) => (
            sym.type_info.clone(),
            sym.attrs.pointer,
            sym.attrs.allocatable,
        ),
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
    bool, // pure
    bool, // elemental
    SymbolAttrs,
);

fn reject_imports_in_disallowed_scope(
    imports: &[ImportStmt],
    scope_name: &str,
    span: crate::lexer::Span,
) -> Result<(), SemaError> {
    if imports.is_empty() {
        return Ok(());
    }
    Err(SemaError {
        span,
        msg: format!("IMPORT is not permitted in {scope_name}"),
    })
}

fn process_imports(
    st: &mut SymbolTable,
    scope_id: ScopeId,
    imports: &[ImportStmt],
    default_policy: HostAssociationPolicy,
    span: crate::lexer::Span,
    host_scope_override: Option<ScopeId>,
) -> Result<(), SemaError> {
    let has_only = imports
        .iter()
        .any(|import| matches!(import, ImportStmt::Only(_)));
    if has_only
        && imports
            .iter()
            .any(|import| !matches!(import, ImportStmt::Only(_)))
    {
        return Err(SemaError {
            span,
            msg: "IMPORT, ONLY cannot be combined with another IMPORT form".into(),
        });
    }
    if imports.len() > 1
        && imports
            .iter()
            .any(|import| matches!(import, ImportStmt::All | ImportStmt::None))
    {
        return Err(SemaError {
            span,
            msg: "IMPORT, ALL and IMPORT, NONE must be the only IMPORT statement in a scope".into(),
        });
    }

    let (host_scope, is_interface_body) = if let Some(host_scope) = host_scope_override {
        (host_scope, false)
    } else {
        let parent = st
            .scope(scope_id)
            .parent
            .expect("subprogram scope has a parent");
        let is_interface_body = st.scope(parent).kind == ScopeKind::Interface;
        let host_scope = if is_interface_body {
            st.scope(parent).parent.expect("interface scope has a host")
        } else {
            parent
        };
        (host_scope, is_interface_body)
    };
    let mut imported_names = HashSet::new();
    for name in imports.iter().flat_map(|import| match import {
        ImportStmt::Default(names) | ImportStmt::Only(names) => names.as_slice(),
        ImportStmt::All | ImportStmt::None => &[],
    }) {
        let key = name.to_ascii_lowercase();
        let direct_symbol = st.lookup_in(host_scope, &key);
        let generic_symbol = st.named_interface_facet_symbol_in_scope(host_scope, &key);
        if direct_symbol.is_none() && generic_symbol.is_none() {
            return Err(SemaError {
                span,
                msg: format!("IMPORT name '{}' does not identify a host entity", name),
            });
        }
        let precedes_interface_body = |symbol: &crate::sema::symtab::Symbol| {
            !is_interface_body
                || symbol.scope != host_scope
                || symbol.defined_at.file_id != span.file_id
                || (symbol.defined_at.start.line, symbol.defined_at.start.col)
                    < (span.start.line, span.start.col)
        };
        if !direct_symbol.is_some_and(precedes_interface_body)
            && !generic_symbol.is_some_and(precedes_interface_body)
        {
            return Err(SemaError {
                span,
                msg: format!(
                    "IMPORT name '{}' must be declared before the interface body",
                    name
                ),
            });
        }
        imported_names.insert(key);
    }

    let control = match imports {
        [] => HostAssociationControl {
            policy: default_policy,
            protection: HostImportProtection::None,
            host_declaration_cutoff: is_interface_body.then_some(span),
            host_scope_override,
        },
        [ImportStmt::All] => HostAssociationControl {
            policy: HostAssociationPolicy::All,
            protection: HostImportProtection::All,
            host_declaration_cutoff: is_interface_body.then_some(span),
            host_scope_override,
        },
        [ImportStmt::None] => HostAssociationControl {
            policy: HostAssociationPolicy::None,
            protection: HostImportProtection::None,
            host_declaration_cutoff: is_interface_body.then_some(span),
            host_scope_override,
        },
        imports
            if imports
                .iter()
                .any(|import| matches!(import, ImportStmt::Default(names) if names.is_empty())) =>
        {
            HostAssociationControl {
                policy: HostAssociationPolicy::All,
                protection: HostImportProtection::Names(imported_names),
                host_declaration_cutoff: is_interface_body.then_some(span),
                host_scope_override,
            }
        }
        imports
            if imports
                .iter()
                .all(|import| matches!(import, ImportStmt::Only(_))) =>
        {
            HostAssociationControl {
                policy: HostAssociationPolicy::Only(imported_names.clone()),
                protection: HostImportProtection::Names(imported_names),
                host_declaration_cutoff: is_interface_body.then_some(span),
                host_scope_override,
            }
        }
        _ => HostAssociationControl {
            policy: match default_policy {
                HostAssociationPolicy::All => HostAssociationPolicy::All,
                HostAssociationPolicy::None | HostAssociationPolicy::Only(_) => {
                    HostAssociationPolicy::Only(imported_names.clone())
                }
            },
            protection: HostImportProtection::Names(imported_names),
            host_declaration_cutoff: is_interface_body.then_some(span),
            host_scope_override,
        },
    };
    st.set_host_association_control(scope_id, control);
    Ok(())
}

pub(super) fn resolve_unit(
    st: &mut SymbolTable,
    unit: &SpannedUnit,
    module_search_paths: &[std::path::PathBuf],
    layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Result<(), SemaError> {
    match &unit.node {
        ProgramUnit::Program {
            name,
            uses,
            imports,
            implicit,
            decls,
            body,
            contains,
        } => {
            reject_imports_in_disallowed_scope(imports, "a main program", unit.span)?;
            let scope_name = name.clone().unwrap_or_else(|| "main".into());
            st.push_scope(ScopeKind::Program(scope_name));
            let scope_id = st.current_scope();
            process_uses(st, uses, module_search_paths, layouts)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            process_namelists(st, body)?;
            detect_statement_functions(st, scope_id, body);
            preload_stmt_uses(st, body, module_search_paths, layouts);
            process_contains(st, contains, module_search_paths, layouts)?;
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::Module {
            name,
            uses,
            imports,
            implicit,
            decls,
            contains,
        } => {
            reject_imports_in_disallowed_scope(imports, "a module", unit.span)?;
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
            prefix,
            bind: _,
            uses,
            imports,
            implicit,
            decls,
            body,
            contains,
        } => {
            let host_scope = st.current_scope();
            if st.scope(host_scope).kind == ScopeKind::Global {
                reject_imports_in_disallowed_scope(imports, "an external subprogram", unit.span)?;
            }
            let default_host_policy = if st.scope(host_scope).kind == ScopeKind::Interface
                && !prefix.iter().any(|item| matches!(item, Prefix::Module))
            {
                HostAssociationPolicy::None
            } else {
                HostAssociationPolicy::All
            };
            let scope_id = st.push_scope(ScopeKind::Subroutine(name.clone()));
            process_imports(st, scope_id, imports, default_host_policy, unit.span, None)?;

            // F2008 §12.6.2.5: separate module procedure body — args
            // are inherited from the parent module's interface block.
            // The parser emits args=[] and prefix=[Module]; sema
            // injects the args from the parent module's procedure
            // scope (populated during interface resolution / .amod
            // load).
            let is_separate_body = args.is_empty()
                && prefix.iter().any(|p| matches!(p, Prefix::Module))
                && matches!(
                    st.scope(st.scope(scope_id).parent.unwrap_or(0)).kind,
                    ScopeKind::Submodule(_)
                );
            if is_separate_body {
                inject_separate_module_procedure_args(st, name, scope_id, unit.span);
            } else {
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
                            const_char_value: None,
                        })?;
                    }
                }
            }
            process_uses(st, uses, module_search_paths, layouts)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            process_namelists(st, body)?;
            detect_statement_functions(st, scope_id, body);
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
            prefix,
            uses,
            imports,
            implicit,
            decls,
            body,
            contains,
        } => {
            let host_scope = st.current_scope();
            if st.scope(host_scope).kind == ScopeKind::Global {
                reject_imports_in_disallowed_scope(imports, "an external subprogram", unit.span)?;
            }
            let default_host_policy = if st.scope(host_scope).kind == ScopeKind::Interface
                && !prefix.iter().any(|item| matches!(item, Prefix::Module))
            {
                HostAssociationPolicy::None
            } else {
                HostAssociationPolicy::All
            };
            let scope_id = st.push_scope(ScopeKind::Function(name.clone()));
            process_imports(st, scope_id, imports, default_host_policy, unit.span, None)?;
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
                        const_char_value: None,
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
                const_char_value: None,
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
            process_namelists(st, body)?;
            detect_statement_functions(st, scope_id, body);
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
            ancestor,
            name,
            uses,
            imports,
            decls,
            contains,
        } => {
            if imports
                .iter()
                .any(|import| matches!(import, ImportStmt::None))
            {
                return Err(SemaError {
                    span: unit.span,
                    msg: "IMPORT, NONE is not permitted in a submodule".into(),
                });
            }
            // Resolve the immediate semantic parent. A direct child can load
            // its ancestor module from .amod; a descendant needs the parent
            // submodule scope retained in this translation unit.
            let parent_scope = if let Some(immediate_parent) = ancestor {
                let immediate_scope = st.find_submodule_scope(parent, immediate_parent);
                if immediate_scope.is_none() && !imports.is_empty() {
                    return Err(SemaError {
                        span: unit.span,
                        msg: format!(
                            "IMPORT in descendant submodule '{}' requires semantic information for parent submodule '{}'",
                            name, immediate_parent
                        ),
                    });
                }
                immediate_scope.or_else(|| {
                    st.find_module_scope(parent)
                        .or_else(|| load_external_module(st, parent, module_search_paths, layouts))
                })
            } else {
                st.find_module_scope(parent)
                    .or_else(|| load_external_module(st, parent, module_search_paths, layouts))
            };
            let scope_id = st.push_scope(ScopeKind::Submodule(name.clone()));
            st.set_submodule_ancestor(scope_id, parent);
            if let Some(pid) = parent_scope {
                process_imports(
                    st,
                    scope_id,
                    imports,
                    HostAssociationPolicy::All,
                    unit.span,
                    Some(pid),
                )?;
            } else if !imports.is_empty() {
                return Err(SemaError {
                    span: unit.span,
                    msg: format!(
                        "cannot apply IMPORT because parent module '{}' was not found",
                        parent
                    ),
                });
            }
            process_uses(st, uses, module_search_paths, layouts)?;
            // Import all immediate-parent symbols into the submodule scope.
            // Per F2008 12.2.3.2: submodules see ALL parent entities,
            // including private ones — that's the whole point of the
            // submodule mechanism (host association).
            if let Some(pid) = parent_scope {
                st.add_use_association(UseAssociation {
                    local_name: String::new(),
                    original_name: String::new(),
                    source_scope: pid,
                    is_submodule_access: true,
                    from_bare_use: true,
                });
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
                        from_bare_use: true,
                    });
                }
            }
            process_decls(st, decls)?;
            process_contains(st, contains, module_search_paths, layouts)?;
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
                            prefix,
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
                            let elemental = prefix
                                .iter()
                                .any(|p| matches!(p, crate::ast::unit::Prefix::Elemental));
                            let pure = elemental
                                || prefix
                                    .iter()
                                    .any(|p| matches!(p, crate::ast::unit::Prefix::Pure));
                            // Capture result rank from the function's
                            // own decls (interface-block bodies declare
                            // the result variable here).
                            let result_attrs_for_iface =
                                function_result_attrs(fn_name, result, decls);
                            outer_refs.push((
                                fn_name.clone(),
                                SymbolKind::Function,
                                ti,
                                arg_names,
                                normalized_bind_name(bind.as_ref(), fn_name),
                                pure,
                                elemental,
                                result_attrs_for_iface,
                            ));
                        }
                        ProgramUnit::Subroutine {
                            name: fn_name,
                            args,
                            bind,
                            prefix,
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
                            let elemental = prefix
                                .iter()
                                .any(|p| matches!(p, crate::ast::unit::Prefix::Elemental));
                            let pure = elemental
                                || prefix
                                    .iter()
                                    .any(|p| matches!(p, crate::ast::unit::Prefix::Pure));
                            outer_refs.push((
                                fn_name.clone(),
                                SymbolKind::Subroutine,
                                None,
                                arg_names,
                                normalized_bind_name(bind.as_ref(), fn_name),
                                pure,
                                elemental,
                                SymbolAttrs::default(),
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
            for (fn_name, kind, ti, arg_names, binding_label, pure, elemental, result_attrs) in
                outer_refs
            {
                let span = unit.span;
                let symbol = Symbol {
                    name: fn_name.clone(),
                    kind: kind.clone(),
                    type_info: ti.clone(),
                    attrs: SymbolAttrs {
                        external: true,
                        binding_label: binding_label.clone(),
                        pure,
                        elemental,
                        allocatable: result_attrs.allocatable,
                        pointer: result_attrs.pointer,
                        result_rank: result_attrs.result_rank,
                        array_spec: result_attrs.array_spec.clone(),
                        ..Default::default()
                    },
                    defined_at: span,
                    scope: st.current_scope(),
                    arg_names: arg_names.clone(),
                    const_value: None,
                    const_char_value: None,
                };
                let key = fn_name.to_ascii_lowercase();
                let had_local_symbol = st.scope(st.current_scope()).symbols.contains_key(&key);
                if let Err(err) = st.define(symbol) {
                    if !had_local_symbol {
                        return Err(err);
                    }
                    let scope_id = st.current_scope();
                    let is_dummy_arg = st.scope(scope_id).arg_order.iter().any(|arg| arg == &key);
                    if is_dummy_arg {
                        if let Some(existing) = st.scope_mut(scope_id).symbols.get_mut(&key) {
                            if existing.kind == SymbolKind::Variable {
                                let mut attrs = existing.attrs.clone();
                                attrs.external = true;
                                attrs.binding_label = binding_label;
                                attrs.pure = pure;
                                attrs.elemental = elemental;
                                attrs.allocatable = result_attrs.allocatable;
                                attrs.pointer = result_attrs.pointer;
                                attrs.result_rank = result_attrs.result_rank;
                                attrs.array_spec = result_attrs.array_spec;
                                existing.kind = kind;
                                existing.type_info = ti;
                                existing.attrs = attrs;
                                existing.arg_names = arg_names;
                            }
                        }
                    }
                }
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
                    let key = generic_name.to_ascii_lowercase();
                    let had_local_symbol = st.scope(st.current_scope()).symbols.contains_key(&key);
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
                        const_char_value: None,
                    });
                    if let Err(err) = define_result {
                        if !had_local_symbol {
                            return Err(err);
                        }
                        if let Some(existing) =
                            st.scope_mut(st.current_scope()).symbols.get_mut(&key)
                        {
                            if existing.kind == SymbolKind::NamedInterface
                                || existing.kind == SymbolKind::DerivedType
                            {
                                merge_specific_names(&mut existing.arg_names, &merged_specifics);
                            } else if matches!(
                                existing.kind,
                                SymbolKind::Function
                                    | SymbolKind::Subroutine
                                    | SymbolKind::ExternalProc
                                    | SymbolKind::ProcedurePointer
                            ) {
                                st.define_same_name_generic_interface(Symbol {
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
                                    const_char_value: None,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn stable_scope_path(st: &SymbolTable, scope_id: ScopeId) -> String {
    let scope = st.scope(scope_id);
    let segment = match &scope.kind {
        ScopeKind::Global => return "global".to_string(),
        ScopeKind::Module(name) | ScopeKind::Submodule(name) => {
            return name.to_ascii_lowercase();
        }
        ScopeKind::Program(name) => format!("program:{}", name.to_ascii_lowercase()),
        ScopeKind::Subroutine(name) => format!("subroutine:{}", name.to_ascii_lowercase()),
        ScopeKind::Function(name) => format!("function:{}", name.to_ascii_lowercase()),
        ScopeKind::DerivedType(name) => format!("type:{}", name.to_ascii_lowercase()),
        ScopeKind::Block => format!("block:{}", scope.id),
        ScopeKind::Interface => format!("interface:{}", scope.id),
        ScopeKind::Forall => format!("forall:{}", scope.id),
        ScopeKind::Associate => format!("associate:{}", scope.id),
        ScopeKind::Critical => format!("critical:{}", scope.id),
    };
    let Some(parent) = scope.parent else {
        return segment;
    };
    format!("{}::{}", stable_scope_path(st, parent), segment)
}

fn register_layout_scopes(
    st: &SymbolTable,
    layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) {
    for scope in st.all_scopes() {
        let parent = if matches!(scope.kind, ScopeKind::Module(_) | ScopeKind::Submodule(_)) {
            Some(0)
        } else {
            scope.parent
        };
        layouts.register_scope(scope.id, parent, stable_scope_path(st, scope.id));
    }
    for scope in st.all_scopes() {
        for assoc in &scope.use_associations {
            if assoc.from_bare_use && assoc.local_name.is_empty() {
                layouts.bind_bare_use_scope(scope.id, assoc.source_scope);
            } else {
                layouts.bind_layout(
                    scope.id,
                    &assoc.local_name,
                    assoc.source_scope,
                    &assoc.original_name,
                );
            }
        }
    }
}

/// Walk all program units and compute layouts for derived types.
pub(crate) fn compute_all_layouts(
    target_layout: crate::target::TargetLayout,
    units: &[SpannedUnit],
    st: &SymbolTable,
    layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) {
    register_layout_scopes(st, layouts);
    let inherited_params = HashMap::new();
    let inherited_char_params = HashMap::new();
    let mut visible_param_cache: HashMap<ScopeId, HashMap<String, i64>> = HashMap::new();
    let mut exported_param_cache: HashMap<ScopeId, HashMap<String, i64>> = HashMap::new();
    let mut visible_char_param_cache: HashMap<ScopeId, HashMap<String, String>> = HashMap::new();
    let mut exported_char_param_cache: HashMap<ScopeId, HashMap<String, String>> = HashMap::new();
    for unit in units {
        let scope_id = find_unit_scope(st, 0, &unit.node).unwrap_or(0);
        collect_derived_type_layouts(
            target_layout,
            &unit.node,
            scope_id,
            st,
            layouts,
            &inherited_params,
            &inherited_char_params,
            &mut visible_param_cache,
            &mut exported_param_cache,
            &mut visible_char_param_cache,
            &mut exported_char_param_cache,
        );
    }
    layouts.rebuild_alias_index();
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

/// Inject dummy-argument symbols into a separate-module-procedure body
/// scope (F2008 §12.6.2.5).  The body's parent submodule was already
/// linked to its parent module via UseAssociation; we walk that link
/// to find the parent module's scope, locate the matching
/// `Subroutine(name)` / `Function(name)` child scope (created either
/// by the parent module's interface block resolution or by .amod
/// loading), and clone its argument symbols into the body scope.
fn inject_separate_module_procedure_args(
    st: &mut SymbolTable,
    proc_name: &str,
    body_scope: ScopeId,
    span: crate::lexer::Span,
) {
    let submodule_id = match st.scope(body_scope).parent {
        Some(p) => p,
        None => return,
    };
    let parent_module_scope = st
        .scope(submodule_id)
        .use_associations
        .iter()
        .find(|u| u.is_submodule_access)
        .map(|u| u.source_scope);
    let Some(parent_module_scope) = parent_module_scope else {
        return;
    };

    // Find the matching procedure scope inside the parent module.
    // F2008-style submodules declare the procedure inside an explicit
    // `interface ... end interface` block at module scope, which adds
    // an intermediate Interface scope between the module and the
    // procedure scope. Tolerate one Interface hop.
    let proc_lc = proc_name.to_lowercase();
    let iface_scope = st.all_scopes().iter().find_map(|scope| {
        let direct_parent_matches = scope.parent == Some(parent_module_scope);
        let via_interface = scope
            .parent
            .map(|pid| {
                matches!(st.scope(pid).kind, ScopeKind::Interface)
                    && st.scope(pid).parent == Some(parent_module_scope)
            })
            .unwrap_or(false);
        if !direct_parent_matches && !via_interface {
            return None;
        }
        match &scope.kind {
            ScopeKind::Subroutine(n) | ScopeKind::Function(n)
                if n.eq_ignore_ascii_case(&proc_lc) =>
            {
                Some(scope.id)
            }
            _ => None,
        }
    });
    let Some(iface_scope) = iface_scope else {
        return;
    };

    // Snapshot arg_order and dummy-arg symbols from the interface
    // scope before mutating the body scope.
    let arg_order = st.scope(iface_scope).arg_order.clone();
    let arg_symbols: Vec<Symbol> = arg_order
        .iter()
        .filter_map(|n| {
            st.scope(iface_scope).symbols.get(n).cloned().or_else(|| {
                st.scope(iface_scope)
                    .symbols
                    .iter()
                    .find(|(_, s)| s.name.eq_ignore_ascii_case(n))
                    .map(|(_, s)| s.clone())
            })
        })
        .collect();

    // Sprint35-SMP Phase 2: also clone the result variable (function
    // case) so the body's `res = ...` references resolve to a
    // properly-typed Variable rather than implicit-typing as a scalar.
    // The result variable is the non-arg Variable in the iface scope:
    //   - For interface bodies parsed from source: sema's Function arm
    //     defined a Symbol with the user's `result(NAME)` clause.
    //   - For .amod-loaded modules: load_external_module synthesized
    //     a `__amod_result_NAME` Variable that we strip the prefix off.
    let result_sym: Option<Symbol> = {
        let arg_set: std::collections::HashSet<String> = st
            .scope(iface_scope)
            .arg_order
            .iter()
            .map(|n| n.to_lowercase())
            .collect();
        st.scope(iface_scope)
            .symbols
            .iter()
            .find(|(key, sym)| {
                !arg_set.contains(*key)
                    && matches!(sym.kind, SymbolKind::Variable | SymbolKind::Parameter)
            })
            .map(|(_, sym)| sym.clone())
    };

    st.scope_mut(body_scope).arg_order = arg_order;
    for mut sym in arg_symbols {
        sym.scope = body_scope;
        sym.defined_at = span;
        let _ = st.define(sym);
    }
    if let Some(mut sym) = result_sym {
        sym.scope = body_scope;
        sym.defined_at = span;
        // For .amod-loaded result vars the name carries the
        // `__amod_result_` prefix to avoid shadowing user locals in
        // the parent module's procedure scope. Strip it for the body
        // scope so user code referencing the result by its declared
        // name resolves correctly.
        if let Some(stripped) = sym.name.strip_prefix("__amod_result_") {
            sym.name = stripped.to_string();
        }
        let _ = st.define(sym);
    }
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
    exported_cache.insert(scope_id, HashMap::new());

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
        if !st.association_allowed_from_scope(scope_id, assoc, &assoc.local_name) {
            continue;
        }
        // First try the source scope's own symbols, then chase through
        // its USE chain.  stdlib_kinds re-exports `int32` from
        // iso_fortran_env, so `use stdlib_kinds, only: bits_kind => int32`
        // can't find `int32` in stdlib_kinds's own symbol table — it
        // lives one hop further up.  Without the chase, kind selectors
        // resolve to None and downstream layout falls back to default
        // kind, which silently shrinks `integer(block_kind) :: blk`
        // from 8 bytes to 4 inside derived types.
        let direct_value = st
            .scope(assoc.source_scope)
            .symbols
            .get(&assoc.original_name)
            .and_then(|sym| {
                (sym.attrs.parameter
                    && (sym.attrs.access != Access::Private || assoc.is_submodule_access))
                    .then_some(sym.const_value)
                    .flatten()
            });
        if let Some(value) = direct_value {
            out.entry(assoc.local_name.clone()).or_insert(value);
            continue;
        }
        if assoc.is_submodule_access {
            if let Some(sym) = st.lookup_in(assoc.source_scope, &assoc.original_name) {
                if sym.attrs.parameter {
                    if let Some(value) = sym.const_value {
                        out.entry(assoc.local_name.clone()).or_insert(value);
                    }
                }
            }
        } else if let Some(value) =
            exported_const_int_params(st, assoc.source_scope, _visible_cache, exported_cache)
                .get(&assoc.original_name)
                .copied()
        {
            out.entry(assoc.local_name.clone()).or_insert(value);
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
            if !st.association_allowed_from_scope(scope_id, assoc, &name) {
                continue;
            }
            out.entry(name).or_insert(value);
        }
    }

    exported_cache.insert(scope_id, out.clone());
    out
}

fn exported_const_char_params(
    st: &SymbolTable,
    scope_id: ScopeId,
    _visible_cache: &mut HashMap<ScopeId, HashMap<String, String>>,
    exported_cache: &mut HashMap<ScopeId, HashMap<String, String>>,
) -> HashMap<String, String> {
    if let Some(cached) = exported_cache.get(&scope_id) {
        return cached.clone();
    }
    exported_cache.insert(scope_id, HashMap::new());

    let scope = st.scope(scope_id);
    let mut out = HashMap::new();

    for (name, sym) in &scope.symbols {
        if sym.attrs.parameter && sym.attrs.access != Access::Private {
            if let Some(value) = &sym.const_char_value {
                out.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
    }

    for assoc in &scope.use_associations {
        if !st.association_allowed_from_scope(scope_id, assoc, &assoc.local_name) {
            continue;
        }
        let direct_value = st
            .scope(assoc.source_scope)
            .symbols
            .get(&assoc.original_name)
            .and_then(|sym| {
                if sym.attrs.parameter
                    && (sym.attrs.access != Access::Private || assoc.is_submodule_access)
                {
                    sym.const_char_value.clone()
                } else {
                    None
                }
            });
        if let Some(value) = direct_value {
            out.entry(assoc.local_name.clone()).or_insert(value);
            continue;
        }
        if assoc.is_submodule_access {
            if let Some(sym) = st.lookup_in(assoc.source_scope, &assoc.original_name) {
                if sym.attrs.parameter {
                    if let Some(value) = &sym.const_char_value {
                        out.entry(assoc.local_name.clone())
                            .or_insert_with(|| value.clone());
                    }
                }
            }
        } else if let Some(value) =
            exported_const_char_params(st, assoc.source_scope, _visible_cache, exported_cache)
                .get(&assoc.original_name)
                .cloned()
        {
            out.entry(assoc.local_name.clone()).or_insert(value);
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
            exported_const_char_params(st, assoc.source_scope, _visible_cache, exported_cache)
        {
            if !st.association_allowed_from_scope(scope_id, assoc, &name) {
                continue;
            }
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
        if !st.association_allowed_from_scope(scope_id, assoc, &assoc.local_name) {
            continue;
        }
        if out.contains_key(&assoc.local_name) {
            continue;
        }
        let direct_value = st
            .scope(assoc.source_scope)
            .symbols
            .get(&assoc.original_name)
            .and_then(|sym| {
                (sym.attrs.parameter
                    && (sym.attrs.access != Access::Private || assoc.is_submodule_access))
                    .then_some(sym.const_value)
                    .flatten()
            });
        if let Some(value) = direct_value {
            out.insert(assoc.local_name.clone(), value);
            continue;
        }
        if assoc.is_submodule_access {
            if let Some(sym) = st.lookup_in(assoc.source_scope, &assoc.original_name) {
                if sym.attrs.parameter {
                    if let Some(value) = sym.const_value {
                        out.insert(assoc.local_name.clone(), value);
                    }
                }
            }
        } else if let Some(value) =
            exported_const_int_params(st, assoc.source_scope, visible_cache, exported_cache)
                .get(&assoc.original_name)
                .copied()
        {
            out.insert(assoc.local_name.clone(), value);
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
            if !st.association_allowed_from_scope(scope_id, assoc, &name) {
                continue;
            }
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

fn visible_const_char_params(
    st: &SymbolTable,
    scope_id: ScopeId,
    visible_cache: &mut HashMap<ScopeId, HashMap<String, String>>,
    exported_cache: &mut HashMap<ScopeId, HashMap<String, String>>,
) -> HashMap<String, String> {
    if let Some(cached) = visible_cache.get(&scope_id) {
        return cached.clone();
    }

    let scope = st.scope(scope_id);
    let mut out = HashMap::new();

    for (name, sym) in &scope.symbols {
        if sym.attrs.parameter {
            if let Some(value) = &sym.const_char_value {
                out.entry(name.clone()).or_insert_with(|| value.clone());
            }
        }
    }

    for assoc in &scope.use_associations {
        if !st.association_allowed_from_scope(scope_id, assoc, &assoc.local_name) {
            continue;
        }
        if out.contains_key(&assoc.local_name) {
            continue;
        }
        let direct_value = st
            .scope(assoc.source_scope)
            .symbols
            .get(&assoc.original_name)
            .and_then(|sym| {
                if sym.attrs.parameter
                    && (sym.attrs.access != Access::Private || assoc.is_submodule_access)
                {
                    sym.const_char_value.clone()
                } else {
                    None
                }
            });
        if let Some(value) = direct_value {
            out.insert(assoc.local_name.clone(), value);
            continue;
        }
        if assoc.is_submodule_access {
            if let Some(sym) = st.lookup_in(assoc.source_scope, &assoc.original_name) {
                if sym.attrs.parameter {
                    if let Some(value) = &sym.const_char_value {
                        out.insert(assoc.local_name.clone(), value.clone());
                    }
                }
            }
        } else if let Some(value) =
            exported_const_char_params(st, assoc.source_scope, visible_cache, exported_cache)
                .get(&assoc.original_name)
                .cloned()
        {
            out.insert(assoc.local_name.clone(), value);
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
            exported_const_char_params(st, assoc.source_scope, visible_cache, exported_cache)
        {
            if !st.association_allowed_from_scope(scope_id, assoc, &name) {
                continue;
            }
            out.entry(name).or_insert(value);
        }
    }

    if let Some(parent) = scope.parent {
        if st.scope(parent).kind != ScopeKind::Global {
            for (name, value) in
                visible_const_char_params(st, parent, visible_cache, exported_cache)
            {
                out.entry(name).or_insert(value);
            }
        }
    }

    visible_cache.insert(scope_id, out.clone());
    out
}

fn collect_derived_type_layouts(
    target_layout: crate::target::TargetLayout,
    unit: &ProgramUnit,
    scope_id: ScopeId,
    st: &SymbolTable,
    layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
    inherited_params: &HashMap<String, i64>,
    inherited_char_params: &HashMap<String, String>,
    visible_param_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
    exported_param_cache: &mut HashMap<ScopeId, HashMap<String, i64>>,
    visible_char_param_cache: &mut HashMap<ScopeId, HashMap<String, String>>,
    exported_char_param_cache: &mut HashMap<ScopeId, HashMap<String, String>>,
) {
    let (decls, contains) = match unit {
        ProgramUnit::Program {
            decls, contains, ..
        }
        | ProgramUnit::Module {
            decls, contains, ..
        }
        | ProgramUnit::Submodule {
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
    let mut seed_char_params = inherited_char_params.clone();
    seed_char_params.extend(visible_const_char_params(
        st,
        scope_id,
        visible_char_param_cache,
        exported_char_param_cache,
    ));
    let const_params = collect_const_int_params(decls, &seed_params, &seed_char_params);
    let const_char_params = collect_const_char_params(decls, &seed_char_params, &const_params);
    let empty_derived_field_inits = HashMap::new();
    register_local_type_layouts(
        target_layout,
        decls,
        host_module,
        st,
        scope_id,
        layouts,
        &const_params,
        &const_char_params,
        &empty_derived_field_inits,
    );
    let const_derived_field_inits =
        collect_const_derived_field_inits(decls, layouts, &const_params, &const_char_params);
    register_local_type_layouts(
        target_layout,
        decls,
        host_module,
        st,
        scope_id,
        layouts,
        &const_params,
        &const_char_params,
        &const_derived_field_inits,
    );
    resolve_proc_pointer_default_targets(st, scope_id, layouts);
    for sub in contains {
        let sub_scope_id = find_unit_scope(st, scope_id, &sub.node).unwrap_or(scope_id);
        collect_derived_type_layouts(
            target_layout,
            &sub.node,
            sub_scope_id,
            st,
            layouts,
            &const_params,
            &const_char_params,
            visible_param_cache,
            exported_param_cache,
            visible_char_param_cache,
            exported_char_param_cache,
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

fn selected_char_kind_value(name: &str) -> i64 {
    let name = name.trim_end_matches(' ');
    if name.eq_ignore_ascii_case("default") || name.eq_ignore_ascii_case("ascii") {
        1
    } else {
        -1
    }
}

fn eval_const_char_expr_with_params(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
) -> Option<String> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::StringLiteral { value, .. } => Some(value.clone()),
        Expr::Name { name } => const_char_params.get(&name.to_lowercase()).cloned(),
        Expr::ParenExpr { inner } => {
            eval_const_char_expr_with_params(inner, const_params, const_char_params)
        }
        Expr::BinaryOp {
            op: crate::ast::expr::BinaryOp::Concat,
            left,
            right,
        } => {
            let mut out = eval_const_char_expr_with_params(left, const_params, const_char_params)?;
            out.push_str(&eval_const_char_expr_with_params(
                right,
                const_params,
                const_char_params,
            )?);
            Some(out)
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            match name.to_lowercase().as_str() {
                "char" | "achar" => {
                    let first_arg = args.first().and_then(|arg| {
                        if let crate::ast::expr::SectionSubscript::Element(expr) = &arg.value {
                            Some(expr)
                        } else {
                            None
                        }
                    })?;
                    let code = eval_const_int_expr_with_params(
                        first_arg,
                        const_params,
                        const_char_params,
                    )?;
                    if !(0..=255).contains(&code) {
                        return None;
                    }
                    Some((code as u8 as char).to_string())
                }
                "new_line" => Some("\n".to_string()),
                "repeat" if args.len() == 2 => {
                    let pattern = match &args[0].value {
                        crate::ast::expr::SectionSubscript::Element(expr) => {
                            eval_const_char_expr_with_params(expr, const_params, const_char_params)?
                        }
                        _ => return None,
                    };
                    let copies = match &args[1].value {
                        crate::ast::expr::SectionSubscript::Element(expr) => {
                            eval_const_int_expr_with_params(expr, const_params, const_char_params)?
                        }
                        _ => return None,
                    };
                    if copies < 0 {
                        return None;
                    }
                    Some(pattern.repeat(copies as usize))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn eval_const_char_expr(expr: &crate::ast::expr::SpannedExpr, st: &SymbolTable) -> Option<String> {
    eval_const_char_expr_in_scope(expr, st, st.current_scope())
}

fn eval_const_char_expr_in_scope(
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    scope_id: ScopeId,
) -> Option<String> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::StringLiteral { value, .. } => Some(value.clone()),
        Expr::Name { name } => {
            let sym = st.lookup_in(scope_id, &name.to_lowercase())?;
            if sym.attrs.parameter {
                sym.const_char_value.clone()
            } else {
                None
            }
        }
        Expr::ParenExpr { inner } => eval_const_char_expr_in_scope(inner, st, scope_id),
        Expr::BinaryOp {
            op: crate::ast::expr::BinaryOp::Concat,
            left,
            right,
        } => {
            let mut out = eval_const_char_expr_in_scope(left, st, scope_id)?;
            out.push_str(&eval_const_char_expr_in_scope(right, st, scope_id)?);
            Some(out)
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            match name.to_lowercase().as_str() {
                "char" | "achar" => {
                    let first_arg = args.first().and_then(|arg| {
                        if let crate::ast::expr::SectionSubscript::Element(expr) = &arg.value {
                            Some(expr)
                        } else {
                            None
                        }
                    })?;
                    let code = eval_const_int_expr_in_scope(first_arg, st, scope_id)?;
                    if !(0..=255).contains(&code) {
                        return None;
                    }
                    Some((code as u8 as char).to_string())
                }
                "new_line" => Some("\n".to_string()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn eval_const_int_expr_with_params(
    expr: &crate::ast::expr::SpannedExpr,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
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
            let v = eval_const_int_expr_with_params(operand, const_params, const_char_params)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-v),
                crate::ast::expr::UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_const_int_expr_with_params(left, const_params, const_char_params)?;
            let r = eval_const_int_expr_with_params(right, const_params, const_char_params)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(l + r),
                crate::ast::expr::BinaryOp::Sub => Some(l - r),
                crate::ast::expr::BinaryOp::Mul => Some(l * r),
                crate::ast::expr::BinaryOp::Div if r != 0 => Some(l / r),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => {
            eval_const_int_expr_with_params(inner, const_params, const_char_params)
        }
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            let first_arg_val = args.first().and_then(|a| {
                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                    eval_const_int_expr_with_params(e, const_params, const_char_params)
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
                "selected_logical_kind" => {
                    let bits = first_arg_val?;
                    Some(if bits <= 8 {
                        1
                    } else if bits <= 16 {
                        2
                    } else if bits <= 32 {
                        4
                    } else if bits <= 64 {
                        8
                    } else if bits <= 128 {
                        16
                    } else {
                        -1
                    })
                }
                "max" | "min" => {
                    let is_max = name.eq_ignore_ascii_case("max");
                    let mut acc: Option<i64> = None;
                    for arg in args {
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        let value =
                            eval_const_int_expr_with_params(e, const_params, const_char_params)?;
                        acc = Some(match acc {
                            None => value,
                            Some(prev) if is_max => prev.max(value),
                            Some(prev) => prev.min(value),
                        });
                    }
                    acc
                }
                "selected_char_kind" => {
                    let arg = args.first()?;
                    let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                        return None;
                    };
                    eval_const_char_expr_with_params(e, const_params, const_char_params)
                        .map(|name| selected_char_kind_value(&name))
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
                        _ => eval_const_int_expr_with_params(e, const_params, const_char_params),
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
    inherited_char_params: &HashMap<String, String>,
) -> HashMap<String, i64> {
    let mut params = inherited_params.clone();
    let mut char_params = inherited_char_params.clone();
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
            params.remove(&key);
            char_params.remove(&key);
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
                if let Some(value) = eval_const_int_expr_with_params(init, &params, &char_params) {
                    params.insert(key, value);
                    changed = true;
                } else if !char_params.contains_key(&key) {
                    if let Some(value) =
                        eval_const_char_expr_with_params(init, &params, &char_params)
                    {
                        char_params.insert(key, value);
                        changed = true;
                    }
                }
            }
        }
    }

    params
}

fn collect_const_char_params(
    decls: &[SpannedDecl],
    inherited_char_params: &HashMap<String, String>,
    const_params: &HashMap<String, i64>,
) -> HashMap<String, String> {
    let mut params = inherited_char_params.clone();
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
                if let Some(value) = eval_const_char_expr_with_params(init, const_params, &params) {
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
    layouts: &crate::sema::type_layout::TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
) -> HashMap<String, crate::sema::type_layout::FieldDefaultInit> {
    use crate::sema::type_layout::{
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
                        const_char_params,
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
                    combined.insert(field_name.to_ascii_lowercase(), (field_name, field_init));
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

#[allow(clippy::too_many_arguments)]
fn register_local_type_layouts(
    target_layout: crate::target::TargetLayout,
    decls: &[SpannedDecl],
    host_module: Option<&str>,
    st: &SymbolTable,
    scope_id: ScopeId,
    layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
    const_params: &HashMap<String, i64>,
    const_char_params: &HashMap<String, String>,
    const_derived_field_inits: &HashMap<String, crate::sema::type_layout::FieldDefaultInit>,
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
            let parent = extends
                .as_ref()
                .and_then(|p| layouts.get_for_scope(scope_id, p))
                .cloned();
            let is_abstract = attrs
                .iter()
                .any(|attr| matches!(attr, crate::ast::decl::TypeAttr::Abstract));
            let owner_path = layouts.scope_path(scope_id).map(str::to_string);
            let mut layout = crate::sema::type_layout::compute_layout_with_attrs_in_scope(
                name,
                host_module,
                Some(scope_id),
                owner_path.as_deref(),
                type_bound_procs,
                final_procs,
                components,
                parent.as_ref(),
                is_abstract,
                layouts,
                const_params,
                const_char_params,
                const_derived_field_inits,
                target_layout,
            );
            for (final_proc, source_name) in layout.final_procs.iter_mut().zip(final_procs) {
                final_proc.rank = final_procedure_rank(st, scope_id, source_name).unwrap_or(0);
            }
            // Don't overwrite a layout that has bound_procs or final_procs with one that doesn't.
            // This handles the case where a subroutine redefines a type without CONTAINS.
            let dominated = layouts
                .get_for_scope(scope_id, &name.to_lowercase())
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

fn final_procedure_rank(st: &SymbolTable, owner_scope: ScopeId, name: &str) -> Option<usize> {
    let proc_scope = st
        .all_scopes()
        .iter()
        .enumerate()
        .find(|(_, scope)| {
            scope.parent == Some(owner_scope)
                && matches!(
                    &scope.kind,
                    ScopeKind::Subroutine(scope_name) | ScopeKind::Function(scope_name)
                        if scope_name.eq_ignore_ascii_case(name)
                )
        })
        .map(|(id, _)| id)?;
    let scope = st.scope(proc_scope);
    let first_arg = scope.arg_order.first()?;
    let symbol = scope.symbols.get(&first_arg.to_lowercase())?;
    let specs = &symbol.attrs.array_spec;
    Some(if specs.is_empty() { 0 } else { specs.len() })
}

/// Resolve procedure-pointer default-init targets stored as bare
/// source-level names into their link-time symbols.  A field declared
/// `procedure(iface), pointer :: fn => default_hasher` lands in the
/// layout with `FieldDefaultInit::ProcedurePointer("default_hasher")`,
/// but `default_hasher` may itself be a USE-rename for a procedure
/// living in a different module — `stdlib_hashmaps` aliases
/// `fnv_1_hasher` from `stdlib_hashmap_wrappers` exactly this way.
/// This pass walks every layout the file just registered, looks each
/// proc-pointer target up in its owning type's host-module scope
/// (chasing USE chains), and rewrites the stored string to the
/// `afs_modproc_<origin_mod>_<proc>` mangle the runtime initializer
/// emits via `global_addr`.  Without it the linker reports an
/// undefined `_afs_modproc_<host_mod>_<alias>` reference at example
/// link.
fn resolve_proc_pointer_default_targets(
    st: &SymbolTable,
    scope_id: ScopeId,
    layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) {
    use crate::sema::type_layout::FieldDefaultInit;

    for layout in layouts.layouts.values_mut() {
        let owner = match layout.owner_module.as_deref() {
            Some(m) => m,
            None => continue,
        };
        let owner_scope = match st.find_module_scope(owner) {
            Some(s) => s,
            None => scope_id,
        };
        for field in layout.fields.iter_mut() {
            let target_name = match &field.default_init {
                Some(FieldDefaultInit::ProcedurePointer(name)) => name.clone(),
                _ => continue,
            };
            let resolved = resolve_proc_pointer_link_symbol(st, owner_scope, &target_name);
            field.default_init = Some(FieldDefaultInit::ProcedurePointer(resolved));
        }
    }
}

/// Walk the symbol table from `from_scope` to find the link-time
/// symbol that the source name refers to.  Module procedures get the
/// `afs_modproc_<origin_mod>_<proc>` mangle keyed on the procedure's
/// declaring module; bare external/intrinsic references fall through
/// unmodified.  USE associations are followed transitively so renames
/// like `default_hasher => fnv_1_hasher` resolve to the underlying
/// procedure's origin module.
fn resolve_proc_pointer_link_symbol(st: &SymbolTable, from_scope: ScopeId, target: &str) -> String {
    let key = target.to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut current_scope = from_scope;
    let mut current_name = key.clone();

    loop {
        if !seen.insert((current_scope, current_name.clone())) {
            break;
        }
        let scope = st.scope(current_scope);

        if let Some(sym) = scope.symbols.get(&current_name) {
            return mangle_link_symbol_for(sym, scope, &current_name);
        }

        let assoc = scope
            .use_associations
            .iter()
            .find(|a| a.local_name == current_name);
        if let Some(assoc) = assoc {
            current_scope = assoc.source_scope;
            current_name = assoc.original_name.to_lowercase();
            continue;
        }

        break;
    }

    target.to_string()
}

fn mangle_link_symbol_for(
    sym: &crate::sema::symtab::Symbol,
    scope: &crate::sema::symtab::Scope,
    name_in_scope: &str,
) -> String {
    use crate::sema::symtab::{ScopeKind, SymbolKind};
    match sym.kind {
        SymbolKind::Function | SymbolKind::Subroutine => match &scope.kind {
            ScopeKind::Module(module_name) | ScopeKind::Submodule(module_name) => format!(
                "afs_modproc_{}_{}",
                module_name.to_lowercase(),
                name_in_scope
            ),
            _ => name_in_scope.to_string(),
        },
        _ => name_in_scope.to_string(),
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
                // Sprint35-SMP Phase 1: snapshot the per-decl `dimension(...)`
                // attribute as an array-spec fallback. Per-entity specs override
                // it (e.g. `real, dimension(10) :: a, b(:,:)` declares a as
                // rank-1 size 10 and b as assumed-shape rank-2).
                let attr_dimension: Option<&Vec<crate::ast::decl::ArraySpec>> =
                    attrs.iter().find_map(|a| {
                        if let crate::ast::decl::Attribute::Dimension(specs) = a {
                            Some(specs)
                        } else {
                            None
                        }
                    });

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
                    // For `character(*), parameter :: name = init`, F2008
                    // §5.3.2 says the parameter's length is taken from
                    // `init`.  type_spec_to_info loses that info because
                    // LenSpec::Star → len=None; recover it here when the
                    // init is a string literal or another character
                    // parameter whose length we already know.
                    let mut entity_type_info = type_info.clone();
                    entity_char_len_to_info(&mut entity_type_info, entity.char_len.as_ref(), st);
                    if sym_attrs.parameter
                        && matches!(&entity_type_info, TypeInfo::Character { len: None, .. })
                    {
                        if let Some(init) = entity.init.as_ref() {
                            let derived_len =
                                derived_char_init_len(&init.node, st).map(|n| n as i64);
                            if let Some(n) = derived_len {
                                if let TypeInfo::Character { len, .. } = &mut entity_type_info {
                                    *len = Some(n);
                                }
                            }
                        }
                    }
                    // Sprint35-SMP Phase 1: per-entity attrs clone so each
                    // entity carries its own array_spec (entity-local spec
                    // wins; otherwise fall back to the decl-level dimension
                    // attribute).
                    let mut entity_attrs = sym_attrs.clone();
                    let entity_array_spec = entity
                        .array_spec
                        .clone()
                        .or_else(|| attr_dimension.cloned())
                        .unwrap_or_default();
                    entity_attrs.array_spec = entity_array_spec;
                    if st.scope(st.current_scope()).symbols.contains_key(&key) {
                        // Symbol already exists (e.g., dummy argument) — update type info.
                        let sym = st
                            .scope_mut(st.current_scope())
                            .symbols
                            .get_mut(&key)
                            .unwrap();
                        sym.kind = kind.clone();
                        sym.type_info = Some(entity_type_info.clone());
                        sym.attrs = entity_attrs.clone();
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
                        let const_char_value = if sym_attrs.parameter {
                            entity
                                .init
                                .as_ref()
                                .and_then(|e| eval_const_char_expr(e, st))
                        } else {
                            None
                        };
                        st.define(Symbol {
                            name: entity.name.clone(),
                            kind: kind.clone(),
                            type_info: Some(entity_type_info.clone()),
                            attrs: entity_attrs.clone(),
                            defined_at: decl.span,
                            scope: st.current_scope(),
                            arg_names: arg_names.clone(),
                            const_value,
                            const_char_value,
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
                    const_char_value: None,
                })?;
            }
            Decl::EnumDef {
                type_name,
                enumerators,
            } => {
                // F2023 R760: a named interoperable enum defines a
                // weakly-typed alias of its integer kind; TYPE(name)
                // declarations resolve through this symbol.
                if let Some(tn) = type_name {
                    st.define(Symbol {
                        name: tn.clone(),
                        kind: SymbolKind::EnumerationType,
                        type_info: Some(TypeInfo::Integer { kind: None }),
                        attrs: SymbolAttrs {
                            access: st.default_access(st.current_scope()),
                            ..Default::default()
                        },
                        defined_at: decl.span,
                        scope: st.current_scope(),
                        arg_names: vec![],
                        const_value: None,
                        const_char_value: None,
                    })?;
                }
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
                        const_char_value: None,
                    })?;
                }
            }
            Decl::EnumerationTypeDef { name, enumerators } => {
                // F2023 7.6.2: the type name, then each enumerator as
                // a typed constant of the enumeration type with its
                // 1-based ordinal — NOT integer parameters (contrast
                // EnumDef above; reusing that flattening would erase
                // all type safety).
                st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::EnumerationType,
                    type_info: Some(TypeInfo::Enumeration(name.clone())),
                    attrs: SymbolAttrs {
                        access: st.default_access(st.current_scope()),
                        ..Default::default()
                    },
                    defined_at: decl.span,
                    scope: st.current_scope(),
                    // Enumerators in declaration order: the constructor
                    // range check (R771) needs the count.
                    arg_names: enumerators.clone(),
                    const_value: None,
                    const_char_value: None,
                })?;
                for (i, ename) in enumerators.iter().enumerate() {
                    st.define(Symbol {
                        name: ename.clone(),
                        kind: SymbolKind::Enumerator,
                        type_info: Some(TypeInfo::Enumeration(name.clone())),
                        attrs: SymbolAttrs {
                            access: st.default_access(st.current_scope()),
                            parameter: true,
                            ..Default::default()
                        },
                        defined_at: decl.span,
                        scope: st.current_scope(),
                        arg_names: vec![],
                        const_value: Some((i + 1) as i64),
                        const_char_value: None,
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

fn process_namelists(st: &mut SymbolTable, body: &[SpannedStmt]) -> Result<(), SemaError> {
    for stmt in body {
        let Stmt::Namelist { groups } = &stmt.node else {
            continue;
        };
        for (name, vars) in groups {
            let key = name.to_lowercase();
            if let Some(existing) = st.scope_mut(st.current_scope()).symbols.get_mut(&key) {
                if existing.kind == SymbolKind::Namelist {
                    merge_specific_names(&mut existing.arg_names, vars);
                    continue;
                }
            }
            st.define(Symbol {
                name: name.clone(),
                kind: SymbolKind::Namelist,
                type_info: None,
                attrs: SymbolAttrs::default(),
                defined_at: stmt.span,
                scope: st.current_scope(),
                arg_names: vars.clone(),
                const_value: None,
                const_char_value: None,
            })?;
        }
    }
    Ok(())
}

fn process_contains(
    st: &mut SymbolTable,
    contains: &[SpannedUnit],
    module_search_paths: &[std::path::PathBuf],
    layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Result<(), SemaError> {
    for unit in contains {
        // Register the subprogram name in the current scope before descending.
        let host_is_submodule =
            matches!(st.scope(st.current_scope()).kind, ScopeKind::Submodule(_));
        match &unit.node {
            ProgramUnit::Subroutine {
                name,
                args,
                prefix,
                bind,
                ..
            } => {
                let elemental = prefix
                    .iter()
                    .any(|p| matches!(p, crate::ast::unit::Prefix::Elemental));
                let pure = elemental
                    || prefix
                        .iter()
                        .any(|p| matches!(p, crate::ast::unit::Prefix::Pure));
                let is_smp = host_is_submodule
                    && prefix
                        .iter()
                        .any(|p| matches!(p, crate::ast::unit::Prefix::Module));
                let attrs = SymbolAttrs {
                    pure,
                    elemental,
                    binding_label: normalized_bind_name(bind.as_ref(), name),
                    is_separate_module_procedure: is_smp,
                    ..Default::default()
                };
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
                let key = name.to_ascii_lowercase();
                let had_local_symbol = st.scope(st.current_scope()).symbols.contains_key(&key);
                let define_result = st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Subroutine,
                    type_info: None,
                    attrs,
                    defined_at: unit.span,
                    scope: st.current_scope(),
                    arg_names,
                    const_value: None,
                    const_char_value: None,
                });
                if let Err(err) = define_result {
                    if !had_local_symbol {
                        return Err(err);
                    }
                }
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
                let fn_is_smp = host_is_submodule
                    && prefix
                        .iter()
                        .any(|p| matches!(p, crate::ast::unit::Prefix::Module));
                let fn_attrs = SymbolAttrs {
                    allocatable: result_attrs.allocatable,
                    pointer: result_attrs.pointer,
                    pure: fn_pure,
                    elemental: fn_elemental,
                    binding_label: normalized_bind_name(bind.as_ref(), name),
                    result_rank: result_attrs.result_rank,
                    is_separate_module_procedure: fn_is_smp,
                    ..Default::default()
                };
                let key = name.to_ascii_lowercase();
                let had_local_symbol = st.scope(st.current_scope()).symbols.contains_key(&key);
                let define_result = st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    type_info: ret_type_info,
                    attrs: fn_attrs,
                    defined_at: unit.span,
                    scope: st.current_scope(),
                    arg_names: vec![],
                    const_value: None,
                    const_char_value: None,
                });
                if let Err(err) = define_result {
                    if !had_local_symbol {
                        return Err(err);
                    }
                }
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
pub(super) fn eval_const_int_expr(
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) -> Option<i64> {
    eval_const_int_expr_in_scope(expr, st, st.current_scope())
}

pub(super) fn eval_const_int_expr_in_scope(
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    scope_id: ScopeId,
) -> Option<i64> {
    use crate::ast::expr::Expr;
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => {
            let clean = text.split('_').next().unwrap_or(text);
            clean.parse::<i64>().ok()
        }
        Expr::BozLiteral { text, base } => parse_boz_i64(text, *base),
        Expr::Name { name } => {
            let sym = st.lookup_in(scope_id, &name.to_lowercase())?;
            if sym.attrs.parameter {
                sym.const_value
            } else {
                None
            }
        }
        Expr::UnaryOp { op, operand } => {
            let v = eval_const_int_expr_in_scope(operand, st, scope_id)?;
            match op {
                crate::ast::expr::UnaryOp::Minus => Some(-v),
                crate::ast::expr::UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_const_int_expr_in_scope(left, st, scope_id)?;
            let r = eval_const_int_expr_in_scope(right, st, scope_id)?;
            match op {
                crate::ast::expr::BinaryOp::Add => Some(l + r),
                crate::ast::expr::BinaryOp::Sub => Some(l - r),
                crate::ast::expr::BinaryOp::Mul => Some(l * r),
                crate::ast::expr::BinaryOp::Div if r != 0 => Some(l / r),
                _ => None,
            }
        }
        Expr::ParenExpr { inner } => eval_const_int_expr_in_scope(inner, st, scope_id),
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                let first_arg_val = args.first().and_then(|a| {
                    if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                        eval_const_int_expr_in_scope(e, st, scope_id)
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
                    "selected_logical_kind" => {
                        let bits = first_arg_val?;
                        Some(if bits <= 8 {
                            1
                        } else if bits <= 16 {
                            2
                        } else if bits <= 32 {
                            4
                        } else if bits <= 64 {
                            8
                        } else if bits <= 128 {
                            16
                        } else {
                            -1
                        })
                    }
                    "max" | "min" => {
                        let is_max = key == "max";
                        let mut acc: Option<i64> = None;
                        for arg in args {
                            let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                                return None;
                            };
                            let value = eval_const_int_expr_in_scope(e, st, scope_id)?;
                            acc = Some(match acc {
                                None => value,
                                Some(prev) if is_max => prev.max(value),
                                Some(prev) => prev.min(value),
                            });
                        }
                        acc
                    }
                    "selected_char_kind" => {
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        eval_const_char_expr_in_scope(e, st, scope_id)
                            .map(|value| selected_char_kind_value(&value))
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
                            _ => eval_const_int_expr_in_scope(e, st, scope_id),
                        }
                    }
                    "range" => {
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        let ty = match &e.node {
                            Expr::Name { name } => st
                                .lookup_in(scope_id, &name.to_lowercase())
                                .and_then(|sym| sym.type_info.as_ref()),
                            Expr::ParenExpr { inner } => match &inner.node {
                                Expr::Name { name } => st
                                    .lookup_in(scope_id, &name.to_lowercase())
                                    .and_then(|sym| sym.type_info.as_ref()),
                                _ => None,
                            },
                            _ => None,
                        }?;
                        match ty {
                            TypeInfo::Integer { kind } => Some(
                                match kind.unwrap_or(crate::driver::defaults::default_int_kind()) {
                                    1 => 2,
                                    2 => 4,
                                    4 => 9,
                                    8 => 18,
                                    16 => 38,
                                    _ => return None,
                                },
                            ),
                            TypeInfo::Real { kind } => Some(
                                match kind.unwrap_or(crate::driver::defaults::default_real_kind()) {
                                    4 => 37,
                                    8 => 307,
                                    _ => return None,
                                },
                            ),
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
            Attribute::NoPass => {}
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
        let matching_entity = entities
            .iter()
            .find(|entity| entity.name.eq_ignore_ascii_case(&result_key));
        if let Some(entity) = matching_entity {
            let mut sym_attrs = attrs_to_symbol_attrs(attrs, Access::Default);
            // Capture result rank: prefer the entity-local array_spec
            // (e.g. `real :: w(:)`), falling back to a `dimension(...)`
            // attribute on the type-decl statement.
            let rank_from_entity = entity.array_spec.as_ref().map(|specs| specs.len());
            let rank_from_attrs = attrs.iter().find_map(|a| match a {
                crate::ast::decl::Attribute::Dimension(specs) => Some(specs.len()),
                _ => None,
            });
            sym_attrs.result_rank = rank_from_entity
                .or(rank_from_attrs)
                .unwrap_or(0)
                .min(u8::MAX as usize) as u8;
            return sym_attrs;
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
        resolve_file(&units, &[], crate::target::TargetLayout::LP64)
            .unwrap()
            .st
    }

    fn resolve_source_with_layouts(src: &str) -> ResolveResult {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        resolve_file(&units, &[], crate::target::TargetLayout::LP64).unwrap()
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
    fn same_named_module_layouts_resolve_through_use_renames() {
        let resolved = resolve_source_with_layouts(
            "\
module alpha_m
  implicit none
  type :: item_t
    integer :: alpha
  end type
end module
module beta_m
  implicit none
  type :: item_t
    real(8) :: beta
  end type
end module
program p
  use alpha_m, only: alpha_item => item_t
  use beta_m, only: beta_item => item_t
  implicit none
  type(alpha_item) :: left
  type(beta_item) :: right
end program
",
        );
        let program_scope = resolved
            .st
            .all_scopes()
            .iter()
            .find(|scope| matches!(scope.kind, ScopeKind::Program(_)))
            .unwrap()
            .id;

        let alpha = resolved
            .type_layouts
            .get_for_scope(program_scope, "alpha_item")
            .unwrap();
        let beta = resolved
            .type_layouts
            .get_for_scope(program_scope, "beta_item")
            .unwrap();
        assert_eq!(alpha.owner_module.as_deref(), Some("alpha_m"));
        assert_eq!(beta.owner_module.as_deref(), Some("beta_m"));
        assert_eq!(alpha.size, 4);
        assert_eq!(beta.size, 8);
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
