//! USE-statement resolution.
//!
//! Extracted from `core.rs` in Sprint 14. Contains the four functions
//! that handle USE association: `process_uses` (the main entry point),
//! `preload_stmt_uses` and `load_external_module`
//! (the .amod loader that synthesises a module scope when a USE'd
//! module wasn't seen in-file).

use crate::ast::decl::{ArraySpec, Decl, OnlyItem, SpannedDecl, UseNature};
use crate::ast::expr::Expr;
use crate::sema::symtab::*;
use std::collections::HashSet;

use super::core::{
    backfill_procedure_interfaces, merge_specific_names, resolve_unit, LOADED_EXTERNAL_MODULES,
    LOADING_EXTERNAL_SUBMODULES,
};

fn add_use_association(
    st: &mut SymbolTable,
    association: UseAssociation,
    span: crate::lexer::Span,
) -> Result<(), SemaError> {
    if !association.local_name.is_empty()
        && st.current_use_name_conflicts_with_import(&association.local_name)
    {
        return Err(SemaError {
            span,
            msg: format!(
                "USE association '{}' conflicts with an explicitly imported host entity",
                association.local_name
            ),
        });
    }
    st.add_use_association(association);
    Ok(())
}

fn resolve_module_scope(
    st: &mut SymbolTable,
    module: &str,
    nature: UseNature,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<ScopeId> {
    match nature {
        UseNature::Normal => st
            .find_non_intrinsic_module_scope(module)
            .or_else(|| load_external_module(st, module, search_paths, type_layouts))
            .or_else(|| st.find_intrinsic_module_scope(module)),
        UseNature::Intrinsic => st.find_intrinsic_module_scope(module),
        UseNature::NonIntrinsic => st
            .find_non_intrinsic_module_scope(module)
            .or_else(|| load_external_module(st, module, search_paths, type_layouts)),
    }
}

fn install_procedure_interface_import(
    st: &mut SymbolTable,
    procedure_scope: ScopeId,
    import: &crate::sema::amod::UseRename,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) {
    let Some(source_scope) = resolve_module_scope(
        st,
        &import.source_module,
        import.source_nature,
        search_paths,
        type_layouts,
    ) else {
        return;
    };
    let already_present = st
        .scope(procedure_scope)
        .use_associations
        .iter()
        .any(|association| {
            association.source_scope == source_scope
                && association.local_name.eq_ignore_ascii_case(&import.local)
                && association
                    .original_name
                    .eq_ignore_ascii_case(&import.original)
        });
    if already_present {
        return;
    }
    st.enter_scope(procedure_scope);
    st.add_use_association(UseAssociation {
        local_name: import.local.clone(),
        original_name: import.original.clone(),
        source_scope,
        is_submodule_access: false,
        from_bare_use: false,
    });
}

pub(super) fn process_uses(
    st: &mut SymbolTable,
    uses: &[SpannedDecl],
    module_search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Result<(), SemaError> {
    for use_decl in uses {
        if let Decl::UseStmt {
            module,
            nature,
            renames,
            only,
        } = &use_decl.node
        {
            let mod_scope =
                resolve_module_scope(st, module, *nature, module_search_paths, type_layouts);
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
                                add_use_association(
                                    st,
                                    UseAssociation {
                                        local_name: name.clone(),
                                        original_name: name.clone(),
                                        source_scope: mod_scope,
                                        is_submodule_access: false,
                                        from_bare_use: false,
                                    },
                                    use_decl.span,
                                )?;
                            }
                            OnlyItem::Generic(name) => {
                                add_use_association(
                                    st,
                                    UseAssociation {
                                        local_name: name.clone(),
                                        original_name: name.clone(),
                                        source_scope: mod_scope,
                                        is_submodule_access: false,
                                        from_bare_use: false,
                                    },
                                    use_decl.span,
                                )?;
                            }
                            OnlyItem::Rename(rename) => {
                                add_use_association(
                                    st,
                                    UseAssociation {
                                        local_name: rename.local.clone(),
                                        original_name: rename.remote.clone(),
                                        source_scope: mod_scope,
                                        is_submodule_access: false,
                                        from_bare_use: false,
                                    },
                                    use_decl.span,
                                )?;
                            }
                        }
                    }
                } else {
                    // USE without ONLY: import all public symbols.
                    //
                    // Keep a bare module edge even when the producer has no
                    // local symbols. Empty façade modules such as
                    // stdlib_sparse re-export through their USE chain; without
                    // this edge the consumer has no source scope to walk.
                    add_use_association(
                        st,
                        UseAssociation {
                            local_name: String::new(),
                            original_name: String::new(),
                            source_scope: mod_scope,
                            is_submodule_access: false,
                            from_bare_use: true,
                        },
                        use_decl.span,
                    )?;
                    let mod_symbols: Vec<(String, String)> = st
                        .scope(mod_scope)
                        .symbols
                        .iter()
                        .filter(|(_, sym)| sym.attrs.access != Access::Private)
                        .map(|(key, sym)| (sym.name.clone(), key.clone()))
                        .collect();
                    for (name, _key) in &mod_symbols {
                        add_use_association(
                            st,
                            UseAssociation {
                                local_name: name.clone(),
                                original_name: name.clone(),
                                source_scope: mod_scope,
                                is_submodule_access: false,
                                from_bare_use: true,
                            },
                            use_decl.span,
                        )?;
                    }
                    // Apply renames. Renames inside a bare USE rebind a
                    // single name; the name itself is no longer bare so
                    // it doesn't extend transitive lookup.
                    for rename in renames {
                        add_use_association(
                            st,
                            UseAssociation {
                                local_name: rename.local.clone(),
                                original_name: rename.remote.clone(),
                                source_scope: mod_scope,
                                is_submodule_access: false,
                                from_bare_use: true,
                            },
                            use_decl.span,
                        )?;
                    }
                }
            } else {
                let msg = match nature {
                    UseNature::Normal => format!(
                        "module '{}' not found (searched -I paths and current directory for {}.amod)",
                        module,
                        module.to_lowercase()
                    ),
                    UseNature::Intrinsic => {
                        format!("module '{}' is not an intrinsic module", module)
                    }
                    UseNature::NonIntrinsic => format!(
                        "non-intrinsic module '{}' not found (searched -I paths and current directory for {}.amod)",
                        module,
                        module.to_lowercase()
                    ),
                };
                return Err(SemaError {
                    msg,
                    span: use_decl.span,
                });
            }
            if let Some((name, span)) = st.current_local_use_conflict() {
                return Err(SemaError {
                    span,
                    msg: format!(
                        "local declaration '{}' conflicts with a USE-associated entity",
                        name
                    ),
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
pub(super) fn preload_stmt_uses(
    st: &mut SymbolTable,
    stmts: &[crate::ast::stmt::SpannedStmt],
    module_search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
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
                let block_scope = st.push_scope(ScopeKind::Block);
                st.register_statement_block_scope(stmt.span, block_scope);
                let _ = process_uses(st, uses, module_search_paths, type_layouts);
                for iface in ifaces {
                    let _ = resolve_unit(st, iface, module_search_paths, type_layouts);
                }
                preload_stmt_uses(st, body, module_search_paths, type_layouts);
                st.pop_scope();
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

struct SubmoduleRecord {
    parent_submodule: Option<String>,
    interface_name: String,
    interface_fingerprint: String,
}

fn find_module_artifact(
    filename: &str,
    search_paths: &[std::path::PathBuf],
) -> Option<std::path::PathBuf> {
    search_paths
        .iter()
        .map(|path| path.join(filename))
        .chain(std::iter::once(std::path::PathBuf::from(filename)))
        .find(|path| path.exists())
}

fn read_submodule_record(
    path: &std::path::Path,
    ancestor: &str,
    name: &str,
) -> Result<SubmoduleRecord, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {}", path.display(), error))?;
    let version = content
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!smod "))
        .and_then(|value| value.trim().parse::<u32>().ok())
        .ok_or_else(|| format!("{}: invalid .smod header", path.display()))?;
    if version != crate::sema::amod::SMOD_VERSION {
        return Err(format!(
            "{}: incompatible .smod version {} (compiler requires {}); rebuild the parent submodule",
            path.display(),
            version,
            crate::sema::amod::SMOD_VERSION
        ));
    }

    let parent = content
        .lines()
        .find_map(|line| line.strip_prefix("@parent "))
        .map(str::trim)
        .ok_or_else(|| format!("{}: missing @parent record", path.display()))?;
    let (recorded_ancestor, parent_submodule) = match parent.split_once(':') {
        Some((root, immediate)) if !immediate.trim().is_empty() => {
            (root.trim(), Some(immediate.trim().to_ascii_lowercase()))
        }
        Some(_) => {
            return Err(format!("{}: malformed @parent record", path.display()));
        }
        None => (parent, None),
    };
    if !recorded_ancestor.eq_ignore_ascii_case(ancestor) {
        return Err(format!(
            "{}: submodule ancestor '{}' does not match expected '{}'",
            path.display(),
            recorded_ancestor,
            ancestor
        ));
    }

    let recorded_name = content
        .lines()
        .find_map(|line| line.strip_prefix("@submodule "))
        .map(str::trim)
        .ok_or_else(|| format!("{}: missing @submodule record", path.display()))?;
    if !recorded_name.eq_ignore_ascii_case(name) {
        return Err(format!(
            "{}: submodule name '{}' does not match expected '{}'",
            path.display(),
            recorded_name,
            name
        ));
    }

    let interface_record = content
        .lines()
        .find_map(|line| line.strip_prefix("@interface "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{}: missing @interface record", path.display()))?;
    let mut interface_parts = interface_record.split_whitespace();
    let interface_name = interface_parts
        .next()
        .ok_or_else(|| format!("{}: malformed @interface record", path.display()))?;
    let interface_fingerprint = interface_parts
        .next()
        .and_then(|value| value.strip_prefix("fnv1a:"))
        .filter(|value| value.len() == 16 && value.chars().all(|ch| ch.is_ascii_hexdigit()))
        .ok_or_else(|| format!("{}: malformed @interface checksum", path.display()))?;
    if interface_parts.next().is_some() {
        return Err(format!("{}: malformed @interface record", path.display()));
    }
    let expected_interface = format!(
        "{}@{}.amod",
        ancestor.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    );
    if interface_name != expected_interface {
        return Err(format!(
            "{}: interface '{}' does not match expected '{}'",
            path.display(),
            interface_name,
            expected_interface
        ));
    }

    Ok(SubmoduleRecord {
        parent_submodule,
        interface_name: interface_name.to_string(),
        interface_fingerprint: interface_fingerprint.to_ascii_lowercase(),
    })
}

pub(super) fn load_external_submodule(
    st: &mut SymbolTable,
    ancestor: &str,
    name: &str,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<ScopeId> {
    if let Some(scope_id) = st.find_submodule_scope(ancestor, name) {
        return Some(scope_id);
    }

    let key = (ancestor.to_ascii_lowercase(), name.to_ascii_lowercase());
    let inserted = LOADING_EXTERNAL_SUBMODULES.with(|cell| cell.borrow_mut().insert(key.clone()));
    if !inserted {
        eprintln!(
            "warning: cyclic submodule parent metadata involving '{}:{}'",
            ancestor, name
        );
        return None;
    }
    let loaded = load_external_submodule_inner(st, ancestor, name, search_paths, type_layouts);
    LOADING_EXTERNAL_SUBMODULES.with(|cell| {
        cell.borrow_mut().remove(&key);
    });
    loaded
}

fn load_external_submodule_inner(
    st: &mut SymbolTable,
    ancestor: &str,
    name: &str,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<ScopeId> {
    let artifact_stem = format!(
        "{}@{}",
        ancestor.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    );
    let smod_path = find_module_artifact(&format!("{}.smod", artifact_stem), search_paths)?;
    let record = match read_submodule_record(&smod_path, ancestor, name) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("warning: {}", error);
            return None;
        }
    };
    let interface_path = smod_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(&record.interface_name);
    let interface_text = match std::fs::read_to_string(&interface_path) {
        Ok(content) => content,
        Err(error) => {
            eprintln!(
                "warning: cannot read {}: {}",
                interface_path.display(),
                error
            );
            return None;
        }
    };
    let actual_fingerprint = crate::sema::amod::artifact_fingerprint(&interface_text);
    if actual_fingerprint != record.interface_fingerprint {
        eprintln!(
            "warning: {}: semantic interface for immediate parent submodule '{}:{}' does not match the checksum in {}; rebuild the parent submodule",
            interface_path.display(),
            ancestor,
            name,
            smod_path.display()
        );
        return None;
    }
    let iface = match crate::sema::amod::read_amod_content(&interface_text, &interface_path) {
        Ok(iface) => iface,
        Err(error) => {
            eprintln!("warning: {}", error);
            return None;
        }
    };
    if !iface.module_name.eq_ignore_ascii_case(name)
        || iface
            .submodule_ancestor
            .as_deref()
            .is_none_or(|root| !root.eq_ignore_ascii_case(ancestor))
        || iface
            .submodule_parent
            .as_deref()
            .map(str::to_ascii_lowercase)
            != record.parent_submodule
    {
        eprintln!(
            "warning: {}: semantic interface does not match submodule record; rebuild the parent submodule",
            interface_path.display()
        );
        return None;
    }

    let loaded = install_external_interface(
        st,
        &artifact_stem,
        iface,
        &interface_path,
        search_paths,
        type_layouts,
    )?;
    st.find_submodule_scope(ancestor, name)
        .filter(|scope_id| *scope_id == loaded)
}

/// Try to load a module interface from an .amod file on the search path.
/// Creates a synthetic module or submodule scope and returns its ID.
pub(super) fn load_external_module(
    st: &mut SymbolTable,
    module_name: &str,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<ScopeId> {
    use crate::sema::amod;

    let filename = format!("{}.amod", module_name.to_lowercase());
    let amod_path = find_module_artifact(&filename, search_paths)?;

    let iface = match amod::read_amod(&amod_path) {
        Ok(iface) => iface,
        Err(e) => {
            eprintln!("warning: {}", e);
            return None;
        }
    };

    install_external_interface(
        st,
        module_name,
        iface,
        &amod_path,
        search_paths,
        type_layouts,
    )
}

fn install_external_interface(
    st: &mut SymbolTable,
    module_name: &str,
    iface: crate::sema::amod::ModuleInterface,
    amod_path: &std::path::Path,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<ScopeId> {
    use crate::lexer::{Position, Span};
    use crate::sema::amod;

    let dummy_span = Span {
        file_id: 0,
        start: Position { line: 0, col: 0 },
        end: Position { line: 0, col: 0 },
    };

    let semantic_parent = if let Some(ancestor) = &iface.submodule_ancestor {
        let parent_scope = if let Some(parent) = &iface.submodule_parent {
            st.find_submodule_scope(ancestor, parent).or_else(|| {
                load_external_submodule(st, ancestor, parent, search_paths, type_layouts)
            })
        } else {
            st.find_module_scope(ancestor)
                .or_else(|| load_external_module(st, ancestor, search_paths, type_layouts))
        };
        let Some(parent_scope) = parent_scope else {
            eprintln!(
                "warning: {}: cannot resolve semantic parent for submodule '{}:{}'",
                amod_path.display(),
                ancestor,
                iface.module_name
            );
            return None;
        };
        Some((ancestor.clone(), parent_scope))
    } else {
        None
    };

    let scope_kind = if semantic_parent.is_some() {
        ScopeKind::Submodule(iface.module_name.clone())
    } else {
        ScopeKind::Module(iface.module_name.clone())
    };
    let scope_id = st.push_scope(scope_kind);
    let _ = st.set_default_access(iface.default_access);
    for (name, access) in &iface.named_access {
        let _ = st.set_symbol_access(name, *access);
    }
    if let Some((ancestor, parent_scope)) = &semantic_parent {
        st.set_submodule_ancestor(scope_id, ancestor);
        let policy = match &iface.host_association {
            amod::AmodHostAssociation::All => HostAssociationPolicy::All,
            amod::AmodHostAssociation::None => HostAssociationPolicy::None,
            amod::AmodHostAssociation::Only(names) => {
                HostAssociationPolicy::Only(names.iter().cloned().collect())
            }
        };
        st.set_host_association_control(
            scope_id,
            HostAssociationControl {
                policy,
                protection: HostImportProtection::None,
                host_declaration_cutoff: None,
                host_scope_override: Some(*parent_scope),
            },
        );
    }

    // Recursively resolve `@uses` dependencies so transitive USE
    // chains see re-exported symbols. Each dep becomes a
    // UseAssociation on this scope, exactly like `use foo` inside a
    // real source module, which makes lookup_in_guarded walk into
    // the dep's symbols. Without this, `USE amod_middle` where
    // middle does `use amod_base` never sees amod_base's symbols.
    for dep in &iface.dependencies {
        let dep_scope =
            resolve_module_scope(st, &dep.module_name, dep.nature, search_paths, type_layouts);
        if let Some(dep_scope) = dep_scope {
            st.enter_scope(scope_id);
            st.add_use_association(crate::sema::symtab::UseAssociation {
                local_name: String::new(),
                original_name: String::new(),
                source_scope: dep_scope,
                is_submodule_access: false,
                from_bare_use: true,
            });
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
                    from_bare_use: true,
                });
            }
        }
    }

    // Replay ONLY-qualified dependency edges exactly. A facade that imported
    // one provider name must not become a bare re-export after .amod loading.
    for import in &iface.only_imports {
        let src_scope = resolve_module_scope(
            st,
            &import.source_module,
            import.source_nature,
            search_paths,
            type_layouts,
        );
        let Some(src_scope) = src_scope else {
            continue;
        };
        st.enter_scope(scope_id);
        st.add_use_association(crate::sema::symtab::UseAssociation {
            local_name: import.local.clone(),
            original_name: import.original.clone(),
            source_scope: src_scope,
            is_submodule_access: false,
            from_bare_use: false,
        });
    }

    // Replay renames from bare USE edges (`@use_rename a = b from m`).
    for rename in &iface.renames {
        let src_scope = resolve_module_scope(
            st,
            &rename.source_module,
            rename.source_nature,
            search_paths,
            type_layouts,
        );
        let Some(src_scope) = src_scope else {
            continue;
        };
        st.enter_scope(scope_id);
        st.add_use_association(crate::sema::symtab::UseAssociation {
            local_name: rename.local.clone(),
            original_name: rename.original.clone(),
            source_scope: src_scope,
            is_submodule_access: false,
            from_bare_use: true,
        });
    }

    if let Some((_, parent_scope)) = semantic_parent {
        for association in &mut st.scope_mut(scope_id).use_associations {
            if association.source_scope == parent_scope {
                association.is_submodule_access = true;
            }
        }
        let has_broad_host_edge = st
            .scope(scope_id)
            .use_associations
            .iter()
            .any(|association| {
                association.source_scope == parent_scope
                    && association.is_submodule_access
                    && association.local_name.is_empty()
            });
        if !has_broad_host_edge {
            st.add_use_association(crate::sema::symtab::UseAssociation {
                local_name: String::new(),
                original_name: String::new(),
                source_scope: parent_scope,
                is_submodule_access: true,
                from_bare_use: true,
            });
        }
        let parent_symbols: Vec<String> = st
            .scope(parent_scope)
            .symbols
            .values()
            .map(|symbol| symbol.name.clone())
            .collect();
        for symbol_name in parent_symbols {
            let has_host_edge = st
                .scope(scope_id)
                .use_associations
                .iter()
                .any(|association| {
                    association.source_scope == parent_scope
                        && association.is_submodule_access
                        && association.local_name.eq_ignore_ascii_case(&symbol_name)
                        && association.original_name.eq_ignore_ascii_case(&symbol_name)
                });
            if !has_host_edge {
                st.add_use_association(crate::sema::symtab::UseAssociation {
                    local_name: symbol_name.clone(),
                    original_name: symbol_name,
                    source_scope: parent_scope,
                    is_submodule_access: true,
                    from_bare_use: true,
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
        let array_spec = if var.rank == 0 {
            Vec::new()
        } else {
            let template = if var.allocatable || var.pointer {
                ArraySpec::Deferred
            } else {
                ArraySpec::AssumedShape { lower: None }
            };
            vec![template; var.rank]
        };
        let attrs = SymbolAttrs {
            access: var.access,
            allocatable: var.allocatable,
            save: var.save,
            pointer: var.pointer,
            target: var.target,
            volatile: var.volatile,
            parameter: var.is_parameter,
            external: var.proc_pointer,
            array_spec,
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
            const_char_value: var.const_char_value.clone(),
        });
    }

    // Re-register F2023 enumeration types and their typed enumerator
    // constants (mirrors Decl::EnumerationTypeDef in resolve/core.rs;
    // ordinals are positional, 1-based). Without this, `type(color)`
    // in a USEing unit fell to the unknown-derived path and every
    // enum assignment was rejected.
    for (ename, enumerators, access) in &iface.enum_types {
        let _ = st.define(Symbol {
            name: ename.clone(),
            kind: SymbolKind::EnumerationType,
            type_info: Some(TypeInfo::Enumeration(ename.clone())),
            attrs: SymbolAttrs {
                access: *access,
                ..Default::default()
            },
            defined_at: dummy_span,
            scope: scope_id,
            arg_names: enumerators.clone(),
            const_value: None,
            const_char_value: None,
        });
        for (i, member) in enumerators.iter().enumerate() {
            let _ = st.define(Symbol {
                name: member.clone(),
                kind: SymbolKind::Enumerator,
                type_info: Some(TypeInfo::Enumeration(ename.clone())),
                attrs: SymbolAttrs {
                    access: *access,
                    parameter: true,
                    ..Default::default()
                },
                defined_at: dummy_span,
                scope: scope_id,
                arg_names: vec![],
                const_value: Some((i + 1) as i64),
                const_char_value: None,
            });
        }
    }

    // Populate procedures. Each proc is defined as a symbol in the
    // module scope AND given its own Function/Subroutine scope whose
    // symbols carry the argument type_info. The dedicated scope is
    // what `resolve_generic_call` walks to match argument types at
    // call sites — without it, cross-TU generic dispatch sees no
    // candidates and fails.
    for proc in &iface.procedures {
        // Sprint35-SMP Phase 2: rebuild the function result's array_spec
        // from result_rank + result_allocatable/pointer flags so the
        // SMP-body synthesizer can recover the result's shape from a
        // pure .amod load (where the result variable isn't otherwise
        // present as a separate symbol).
        let result_array_spec: Vec<ArraySpec> = if proc.result_rank == 0 {
            Vec::new()
        } else {
            let template = if proc.result_allocatable || proc.result_pointer {
                ArraySpec::Deferred
            } else {
                ArraySpec::AssumedShape { lower: None }
            };
            vec![template; proc.result_rank as usize]
        };
        let attrs = SymbolAttrs {
            access: proc.access,
            allocatable: proc.result_allocatable,
            pointer: proc.result_pointer,
            pure: proc.pure,
            elemental: proc.elemental,
            abstract_interface: proc.abstract_interface,
            is_separate_module_interface: proc.is_separate_module_interface,
            is_separate_module_procedure: proc.is_separate_module_procedure,
            bind_c: proc.bind_c,
            binding_label: proc.binding_label.clone(),
            result_rank: proc.result_rank,
            array_spec: result_array_spec,
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
            const_char_value: None,
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
        st.scope_mut(proc_scope).bind_c = proc.bind_c;
        st.scope_mut(proc_scope).binding_label = proc.binding_label.clone();
        if matches!(proc.kind, crate::sema::symtab::SymbolKind::Function) {
            st.scope_mut(proc_scope).result_name = Some(
                proc.result_name
                    .clone()
                    .unwrap_or_else(|| proc.name.clone()),
            );
        }
        for import in proc
            .args
            .iter()
            .filter_map(|arg| arg.procedure_iface_import.as_ref())
            .chain(proc.result_procedure_iface_import.as_ref())
        {
            install_procedure_interface_import(st, proc_scope, import, search_paths, type_layouts);
        }
        let hidden_char_len_args: HashSet<String> = proc
            .args
            .iter()
            .filter(|arg| arg.hidden)
            .filter_map(|arg| arg.name.strip_suffix("@len"))
            .map(|name| name.to_ascii_lowercase())
            .collect();
        for arg in &proc.args {
            if arg.hidden {
                continue;
            }
            // Sprint35-SMP Phase 1: rebuild a same-rank array_spec from
            // the encoded rank + descriptor/allocatable/pointer flags.
            // Bound expressions are not preserved across .amod boundaries;
            // descriptor-passed dummies can use assumed/deferred placeholders
            // because extents come from the caller's runtime descriptor. Raw
            // explicit-shape dummies must stay explicit-shaped, though: using
            // AssumedShape here makes lowering pass a descriptor to callees
            // whose ABI expects a bare element pointer.
            let array_spec: Vec<ArraySpec> = if let Some(specs) = arg
                .array_spec
                .as_ref()
                .filter(|specs| specs.len() == arg.rank as usize)
            {
                specs.clone()
            } else if arg.assumed_rank {
                vec![ArraySpec::AssumedRank]
            } else if arg.rank == 0 {
                Vec::new()
            } else {
                let template = if arg.allocatable || arg.pointer {
                    ArraySpec::Deferred
                } else if arg.descriptor {
                    ArraySpec::AssumedShape { lower: None }
                } else {
                    ArraySpec::Explicit {
                        lower: None,
                        upper: crate::ast::Spanned::new(
                            Expr::IntegerLiteral {
                                text: "1".to_string(),
                                kind: None,
                            },
                            dummy_span,
                        ),
                    }
                };
                vec![template; arg.rank as usize]
            };
            let arg_attrs = SymbolAttrs {
                intent: arg.intent,
                optional: arg.optional,
                value: arg.value,
                allocatable: arg.allocatable,
                pointer: arg.pointer,
                target: arg.target,
                asynchronous: arg.asynchronous,
                contiguous: arg.contiguous,
                volatile: arg.volatile,
                assumed_length_character: hidden_char_len_args
                    .contains(&arg.name.to_ascii_lowercase()),
                external: arg.external,
                procedure_iface: arg.procedure_iface.clone(),
                array_spec,
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
                const_char_value: None,
            });
        }
        // Sprint35-SMP Phase 2: also define the function's result
        // variable in the proc scope under a name that won't collide
        // with the user's own local declarations. Same-name SMP-body
        // procedures push their own Function scope on top, so the
        // duplicate name `result` would otherwise shadow the local
        // and the validator's lookup would walk to this stale symbol
        // and reject `allocate(result(...))`. Use a doubly-underscored
        // synth name so SMP-body synthesis can find it (via the body
        // scope after sema injection) but no user code can collide.
        //
        // l07: this must fire for SCALAR results too (`result_rank == 0`),
        // not just arrays. Without it a separately-compiled SMP function
        // body (`module function f() result(r)`) never receives r's type
        // from the parent .amod, so r falls to implicit typing — e.g. an
        // integer result named `r` becomes REAL, returned in xmm0 while
        // the caller reads eax (silent garbage). The array-spec logic
        // below already handles rank 0 (empty spec).
        if matches!(proc.kind, crate::sema::symtab::SymbolKind::Function) {
            let synth_name = format!(
                "__amod_result_{}",
                proc.result_name.as_deref().unwrap_or(&proc.name)
            );
            // Sprint35-SMP Phase 3: prefer the .amod-preserved
            // explicit-shape bounds when available so split-file
            // submodule lowering of `res = …` allocates a runtime-shape
            // result in the function prologue. Falls through to the
            // legacy AssumedShape template when bounds aren't in .amod
            // (rank-only, allocatable, or pointer results).
            let parsed_bounds = proc
                .result_array_bounds
                .as_deref()
                .and_then(amod::parse_array_bounds);
            let result_array_spec: Vec<ArraySpec> = if proc.result_rank == 0 {
                Vec::new()
            } else if let Some(specs) = parsed_bounds {
                if specs.len() == proc.result_rank as usize {
                    specs
                } else {
                    let template = if proc.result_allocatable || proc.result_pointer {
                        ArraySpec::Deferred
                    } else {
                        ArraySpec::AssumedShape { lower: None }
                    };
                    vec![template; proc.result_rank as usize]
                }
            } else {
                let template = if proc.result_allocatable || proc.result_pointer {
                    ArraySpec::Deferred
                } else {
                    ArraySpec::AssumedShape { lower: None }
                };
                vec![template; proc.result_rank as usize]
            };
            let result_attrs = SymbolAttrs {
                allocatable: proc.result_allocatable,
                pointer: proc.result_pointer,
                external: proc.result_procedure_iface.is_some(),
                procedure_iface: proc.result_procedure_iface.clone(),
                array_spec: result_array_spec,
                ..Default::default()
            };
            let _ = st.define(Symbol {
                name: synth_name,
                kind: if proc.result_procedure_iface.is_some() {
                    crate::sema::symtab::SymbolKind::ProcedurePointer
                } else {
                    crate::sema::symtab::SymbolKind::Variable
                },
                type_info: proc.return_type.clone(),
                attrs: result_attrs,
                defined_at: dummy_span,
                scope: proc_scope,
                arg_names: vec![],
                const_value: None,
                const_char_value: None,
            });
        }
        backfill_procedure_interfaces(st, proc_scope);
        st.pop_scope();
    }
    backfill_procedure_interfaces(st, scope_id);

    // Register type layouts.
    for amod_type in &iface.types {
        let mut layout = amod_type.layout.clone();
        layout
            .owner_module
            .get_or_insert_with(|| module_name.to_string());
        layout.owner_scope = Some(scope_id);
        layout.owner_path = layout.owner_module.as_deref().map(str::to_ascii_lowercase);
        type_layouts.insert(layout.clone());
        // Also add a DerivedType symbol.
        let attrs = SymbolAttrs {
            access: amod_type.access,
            type_owner_module: layout.owner_module.as_deref().map(str::to_ascii_lowercase),
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
            const_char_value: None,
        });
    }

    // Register named generic interfaces. The specifics list rides
    // in `arg_names` to match how intra-file INTERFACE blocks are
    // stored by process_decls — `resolve_generic_call` reads it
    // when dispatching a call through the generic name. The access
    // attribute is preserved from the .amod so that submodules can
    // dispatch private parent interfaces via host association while
    // ordinary `USE` consumers filter them out (F2018 §11.2.3).
    for iface_def in &iface.interfaces {
        let attrs = SymbolAttrs {
            access: iface_def.access,
            ..Default::default()
        };
        let define_result = st.define(Symbol {
            name: iface_def.name.clone(),
            kind: SymbolKind::NamedInterface,
            type_info: None,
            attrs: attrs.clone(),
            defined_at: dummy_span,
            scope: scope_id,
            arg_names: iface_def.specifics.clone(),
            const_value: None,
            const_char_value: None,
        });
        if define_result.is_err() {
            let key = iface_def.name.to_ascii_lowercase();
            if let Some(existing) = st.scope_mut(scope_id).symbols.get_mut(&key) {
                if existing.kind == SymbolKind::NamedInterface
                    || existing.kind == SymbolKind::DerivedType
                {
                    merge_specific_names(&mut existing.arg_names, &iface_def.specifics);
                } else if matches!(
                    existing.kind,
                    SymbolKind::Function
                        | SymbolKind::Subroutine
                        | SymbolKind::ExternalProc
                        | SymbolKind::ProcedurePointer
                ) {
                    st.define_same_name_generic_interface(Symbol {
                        name: iface_def.name.clone(),
                        kind: SymbolKind::NamedInterface,
                        type_info: None,
                        attrs: attrs.clone(),
                        defined_at: dummy_span,
                        scope: scope_id,
                        arg_names: iface_def.specifics.clone(),
                        const_value: None,
                        const_char_value: None,
                    });
                }
            }
        }
    }

    st.pop_scope();

    // Track the loaded interface so resolve_file can return it.
    LOADED_EXTERNAL_MODULES.with(|cell| cell.borrow_mut().push(iface));

    Some(scope_id)
}
