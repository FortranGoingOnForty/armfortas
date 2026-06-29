//! USE-statement resolution.
//!
//! Extracted from `core.rs` in Sprint 14. Contains the four functions
//! that handle USE association: `process_uses` (the main entry point),
//! `ensure_uses_loaded`, `preload_stmt_uses`, and `load_external_module`
//! (the .amod loader that synthesises a module scope when a USE'd
//! module wasn't seen in-file).

use crate::ast::decl::{ArraySpec, Decl, OnlyItem, SpannedDecl};
use crate::ast::expr::Expr;
use crate::sema::symtab::*;

use super::core::{
    backfill_procedure_pointer_interfaces, merge_specific_names, resolve_unit,
    LOADED_EXTERNAL_MODULES,
};

pub(super) fn process_uses(
    st: &mut SymbolTable,
    uses: &[SpannedDecl],
    module_search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
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
                                    from_bare_use: false,
                                });
                            }
                            OnlyItem::Generic(name) => {
                                st.add_use_association(UseAssociation {
                                    local_name: name.clone(),
                                    original_name: name.clone(),
                                    source_scope: mod_scope,
                                    is_submodule_access: false,
                                    from_bare_use: false,
                                });
                            }
                            OnlyItem::Rename(rename) => {
                                st.add_use_association(UseAssociation {
                                    local_name: rename.local.clone(),
                                    original_name: rename.remote.clone(),
                                    source_scope: mod_scope,
                                    is_submodule_access: false,
                                    from_bare_use: false,
                                });
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
                    st.add_use_association(UseAssociation {
                        local_name: String::new(),
                        original_name: String::new(),
                        source_scope: mod_scope,
                        is_submodule_access: false,
                        from_bare_use: true,
                    });
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
                            from_bare_use: true,
                        });
                    }
                    // Apply renames. Renames inside a bare USE rebind a
                    // single name; the name itself is no longer bare so
                    // it doesn't extend transitive lookup.
                    for rename in renames {
                        st.add_use_association(UseAssociation {
                            local_name: rename.local.clone(),
                            original_name: rename.remote.clone(),
                            source_scope: mod_scope,
                            is_submodule_access: false,
                            from_bare_use: false,
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
pub(super) fn ensure_uses_loaded(
    st: &mut SymbolTable,
    uses: &[SpannedDecl],
    module_search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
) {
    for use_decl in uses {
        if let Decl::UseStmt { module, .. } = &use_decl.node {
            if st.find_module_scope(module).is_none() {
                let _ = load_external_module(st, module, module_search_paths, type_layouts);
            }
        }
    }
}

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
pub(super) fn load_external_module(
    st: &mut SymbolTable,
    module_name: &str,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut crate::sema::type_layout::TypeLayoutRegistry,
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

    // Replay use renames recorded by the writer (`@use_rename a = b from m`).
    // Without this, `use stdlib_kinds, only: block_kind => int64` is lost
    // when stdlib_bitsets is serialized, and submodule bodies can no
    // longer resolve `block_kind` for kind selectors — `integer(block_kind)
    // :: dummy` falls back to default kind=4 and silently truncates a
    // 64-bit local to 32 bits.
    for rename in &iface.renames {
        let src_scope = st.find_module_scope(&rename.source_module).or_else(|| {
            load_external_module(st, &rename.source_module, search_paths, type_layouts)
        });
        let Some(src_scope) = src_scope else {
            continue;
        };
        st.enter_scope(scope_id);
        st.add_use_association(crate::sema::symtab::UseAssociation {
            local_name: rename.local.clone(),
            original_name: rename.original.clone(),
            source_scope: src_scope,
            is_submodule_access: false,
            from_bare_use: false,
        });
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
            access: var.access,
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
            const_char_value: var.const_char_value.clone(),
        });
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
            let array_spec: Vec<ArraySpec> = if arg.rank == 0 {
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
                array_spec: result_array_spec,
                ..Default::default()
            };
            let _ = st.define(Symbol {
                name: synth_name,
                kind: crate::sema::symtab::SymbolKind::Variable,
                type_info: proc.return_type.clone(),
                attrs: result_attrs,
                defined_at: dummy_span,
                scope: proc_scope,
                arg_names: vec![],
                const_value: None,
                const_char_value: None,
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
