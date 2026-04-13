//! Symbol resolution — walks the AST and populates symbol tables.
//!
//! First pass: collect declarations, create scopes, process USE/IMPLICIT.
//! This establishes the symbol table that type checking (Sprint 13) will use.

use std::cell::RefCell;
use crate::ast::unit::*;
use crate::ast::decl;
use crate::ast::decl::{SpannedDecl, Decl, TypeSpec, Attribute, OnlyItem};
use super::symtab::*;

thread_local! {
    /// Track externally loaded module interfaces so resolve_file can
    /// return them to the driver for globals extraction.
    static LOADED_EXTERNAL_MODULES: RefCell<Vec<super::amod::ModuleInterface>> = RefCell::new(Vec::new());
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

pub fn resolve_file(units: &[SpannedUnit], module_search_paths: &[std::path::PathBuf]) -> Result<ResolveResult, SemaError> {
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
    compute_all_layouts(units, &mut layouts);

    Ok(ResolveResult { st, type_layouts: layouts, external_modules })
}

fn resolve_unit(
    st: &mut SymbolTable,
    unit: &SpannedUnit,
    module_search_paths: &[std::path::PathBuf],
    layouts: &mut super::type_layout::TypeLayoutRegistry,
) -> Result<(), SemaError> {
    match &unit.node {
        ProgramUnit::Program { name, uses, imports: _, implicit, decls, body: _, contains } => {
            let scope_name = name.clone().unwrap_or_else(|| "<main>".into());
            st.push_scope(ScopeKind::Program(scope_name));
            process_uses(st, uses, module_search_paths, layouts)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            process_contains(st, contains, module_search_paths, layouts)?;
            st.pop_scope();
        }
        ProgramUnit::Module { name, uses, imports: _, implicit, decls, contains } => {
            // Find the pre-created module scope and enter it.
            if let Some(mod_id) = st.find_module_scope(name) {
                let saved = st.enter_scope(mod_id);

                process_uses(st, uses, module_search_paths, layouts)?;
                process_implicit(st, implicit)?;
                process_decls(st, decls)?;
                process_contains(st, contains, module_search_paths, layouts)?;

                st.enter_scope(saved);
            }
        }
        ProgramUnit::Subroutine { name, args, prefix: _, bind: _, uses, imports: _, implicit, decls, body: _, contains } => {
            let scope_id = st.push_scope(ScopeKind::Subroutine(name.clone()));
            // Store ordered arg names for VALUE lookup by callers.
            st.scope_mut(scope_id).arg_order = args.iter().filter_map(|a| {
                if let DummyArg::Name(n) = a { Some(n.to_lowercase()) } else { None }
            }).collect();
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
            process_contains(st, contains, module_search_paths, layouts)?;
            st.pop_scope();
        }
        ProgramUnit::Function { name, args, result, return_type, bind: _, prefix: _, uses, imports: _, implicit, decls, body: _, contains } => {
            let scope_id = st.push_scope(ScopeKind::Function(name.clone()));
            st.scope_mut(scope_id).arg_order = args.iter().filter_map(|a| {
                if let DummyArg::Name(n) = a { Some(n.to_lowercase()) } else { None }
            }).collect();
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
            process_contains(st, contains, module_search_paths, layouts)?;
            st.pop_scope();
        }
        ProgramUnit::BlockData { name, uses, decls } => {
            let scope_name = name.clone().unwrap_or_else(|| "<block_data>".into());
            st.push_scope(ScopeKind::Program(scope_name));
            process_uses(st, uses, module_search_paths, layouts)?;
            process_decls(st, decls)?;
            st.pop_scope();
        }
        ProgramUnit::Submodule { parent, ancestor: _, name, uses, decls, contains } => {
            // Find the parent module scope and inherit its symbols.
            let parent_scope = st.find_module_scope(parent);
            st.push_scope(ScopeKind::Submodule(name.clone()));
            // Import all parent module symbols into the submodule scope.
            if let Some(pid) = parent_scope {
                let parent_syms: Vec<(String, String)> = st.scope(pid).symbols.iter()
                    .filter(|(_, sym)| sym.attrs.access != Access::Private)
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
            st.pop_scope();
        }
        ProgramUnit::InterfaceBlock { name: _, is_abstract: _, bodies } => {
            st.push_scope(ScopeKind::Interface);
            for body in bodies {
                match body {
                    InterfaceBody::Subprogram(sub) => resolve_unit(st, sub, module_search_paths, layouts)?,
                    InterfaceBody::ModuleProcedure(_names) => {
                        // Module procedures are resolved by name during type checking.
                    }
                }
            }
            st.pop_scope();
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
        if let Decl::UseStmt { module, nature: _, renames, only } = &use_decl.node {
            // If the module isn't defined in-file, try loading from .amod.
            let mod_scope = st.find_module_scope(module).or_else(|| {
                load_external_module(st, module, module_search_paths, type_layouts)
            });
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
                    let mod_symbols: Vec<(String, String)> = st.scope(mod_scope).symbols.iter()
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

/// Try to load a module interface from an .amod file on the search path.
/// Creates a synthetic module scope in the symbol table and returns its ID.
fn load_external_module(
    st: &mut SymbolTable,
    module_name: &str,
    search_paths: &[std::path::PathBuf],
    type_layouts: &mut super::type_layout::TypeLayoutRegistry,
) -> Option<ScopeId> {
    use crate::sema::amod;
    use crate::lexer::{Position, Span};

    let filename = format!("{}.amod", module_name.to_lowercase());

    // Search -I paths then CWD.
    let mut candidates: Vec<std::path::PathBuf> = search_paths.iter()
        .map(|p| p.join(&filename))
        .collect();
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

    // Populate variables and parameters.
    for var in &iface.variables {
        let kind = if var.is_parameter { SymbolKind::Parameter } else { SymbolKind::Variable };
        let mut attrs = SymbolAttrs::default();
        attrs.access = Access::Public;
        attrs.allocatable = var.allocatable;
        attrs.save = var.save;
        attrs.pointer = var.pointer;
        attrs.target = var.target;
        attrs.parameter = var.is_parameter;
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

    // Populate procedures.
    for proc in &iface.procedures {
        let mut attrs = SymbolAttrs::default();
        attrs.access = Access::Public;
        attrs.pure = proc.pure;
        attrs.elemental = proc.elemental;
        let arg_names: Vec<String> = proc.args.iter()
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
            arg_names,
            const_value: None,
        });
    }

    // Register type layouts.
    for layout in &iface.types {
        type_layouts.insert(layout.clone());
        // Also add a DerivedType symbol.
        let mut attrs = SymbolAttrs::default();
        attrs.access = Access::Public;
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

    st.pop_scope();

    // Track the loaded interface so resolve_file can return it.
    LOADED_EXTERNAL_MODULES.with(|cell| cell.borrow_mut().push(iface));

    Some(scope_id)
}

/// Walk all program units and compute layouts for derived types.
fn compute_all_layouts(units: &[SpannedUnit], layouts: &mut super::type_layout::TypeLayoutRegistry) {
    for unit in units {
        collect_derived_type_layouts(&unit.node, layouts);
    }
}

fn collect_derived_type_layouts(unit: &ProgramUnit, layouts: &mut super::type_layout::TypeLayoutRegistry) {
    let decls = match unit {
        ProgramUnit::Program { decls, contains, .. } |
        ProgramUnit::Module { decls, contains, .. } => {
            for sub in contains { collect_derived_type_layouts(&sub.node, layouts); }
            decls
        }
        ProgramUnit::Subroutine { decls, contains, .. } |
        ProgramUnit::Function { decls, contains, .. } => {
            for sub in contains { collect_derived_type_layouts(&sub.node, layouts); }
            decls
        }
        _ => return,
    };
    for decl in decls {
        if let Decl::DerivedTypeDef { name, extends, components, type_bound_procs, final_procs, .. } = &decl.node {
            let parent = extends.as_ref().and_then(|p| layouts.get(p)).cloned();
            let layout = super::type_layout::compute_layout(name, type_bound_procs, final_procs, components, parent.as_ref(), layouts);
            // Don't overwrite a layout that has bound_procs or final_procs with one that doesn't.
            // This handles the case where a subroutine redefines a type without CONTAINS.
            let dominated = layouts.get(&name.to_lowercase())
                .map(|existing| {
                    let existing_has = !existing.bound_procs.is_empty() || !existing.final_procs.is_empty();
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
    for decl in decls {
        match &decl.node {
            Decl::AccessDefault { access } => {
                match access {
                    Attribute::Private => st.set_default_access(Access::Private),
                    Attribute::Public => st.set_default_access(Access::Public),
                    _ => {}
                }
            }
            Decl::TypeDecl { type_spec, attrs, entities } => {
                let type_info = type_spec_to_info(type_spec, st);
                let sym_attrs = attrs_to_symbol_attrs(attrs, st.default_access(st.current_scope()));

                for entity in entities {
                    let kind = if sym_attrs.parameter {
                        SymbolKind::Parameter
                    } else {
                        SymbolKind::Variable
                    };
                    let key = entity.name.to_lowercase();
                    if st.scope(st.current_scope()).symbols.contains_key(&key) {
                        // Symbol already exists (e.g., dummy argument) — update type info.
                        let sym = st.scope_mut(st.current_scope()).symbols.get_mut(&key).unwrap();
                        sym.type_info = Some(type_info.clone());
                        sym.attrs = sym_attrs.clone();
                    } else {
                        // Try to fold PARAMETER initializers to a
                        // compile-time integer so .amod can carry the
                        // value and consumers can inline it.
                        let const_value = if sym_attrs.parameter {
                            entity.init.as_ref().and_then(|e| eval_const_int_expr(e, st))
                        } else { None };
                        st.define(Symbol {
                            name: entity.name.clone(),
                            kind,
                            type_info: Some(type_info.clone()),
                            attrs: sym_attrs.clone(),
                            defined_at: decl.span,
                            scope: st.current_scope(),
                            arg_names: vec![],
                            const_value,
                        })?;
                    }
                }
            }
            Decl::DerivedTypeDef { name, .. } => {
                st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::DerivedType,
                    type_info: None,
                    attrs: SymbolAttrs::default(),
                    defined_at: decl.span,
                    scope: st.current_scope(),
                    arg_names: vec![],
                    const_value: None,
                })?;
            }
            _ => {}
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
            ProgramUnit::Subroutine { name, prefix, .. } => {
                let mut attrs = SymbolAttrs::default();
                attrs.pure = prefix.iter().any(|p| matches!(p, crate::ast::unit::Prefix::Pure));
                attrs.elemental = prefix.iter().any(|p| matches!(p, crate::ast::unit::Prefix::Elemental));
                if attrs.elemental { attrs.pure = true; } // ELEMENTAL implies PURE
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
            ProgramUnit::Function { name, return_type, result, decls, prefix, .. } => {
                let ret_type_info = return_type.as_ref().map(|ts| type_spec_to_info(ts, st))
                    .or_else(|| {
                        // Infer return type from result variable's declaration.
                        let result_name = result.as_deref().unwrap_or(name.as_str());
                        let key = result_name.to_lowercase();
                        for d in decls {
                            if let decl::Decl::TypeDecl { type_spec, entities, .. } = &d.node {
                                for e in entities {
                                    if e.name.to_lowercase() == key {
                                        return Some(type_spec_to_info(type_spec, st));
                                    }
                                }
                            }
                        }
                        None
                    });
                let mut fn_attrs = SymbolAttrs::default();
                fn_attrs.pure = prefix.iter().any(|p| matches!(p, crate::ast::unit::Prefix::Pure));
                fn_attrs.elemental = prefix.iter().any(|p| matches!(p, crate::ast::unit::Prefix::Elemental));
                if fn_attrs.elemental { fn_attrs.pure = true; }
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
        Expr::Name { name } => {
            // Look up the name in the current scope chain.
            let sym = st.lookup_in(st.current_scope(), &name.to_lowercase())?;
            if sym.attrs.parameter {
                sym.const_value
            } else { None }
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
                    st.lookup(&key).and_then(|sym| {
                        sym.const_value.map(|v| v as u8)
                    })
                }
                _ => None,
            }
        }
        None => None,
    }
}

/// Extract character length from a CharSelector.
fn extract_char_len(sel: &Option<decl::CharSelector>) -> Option<i64> {
    use crate::ast::expr::Expr;
    match sel {
        Some(cs) => {
            match &cs.len {
                Some(decl::LenSpec::Expr(e)) => {
                    if let Expr::IntegerLiteral { text, .. } = &e.node {
                        text.parse().ok()
                    } else { None }
                }
                Some(decl::LenSpec::Star) => None, // assumed length
                Some(decl::LenSpec::Colon) => None, // deferred length
                None => None,
            }
        }
        None => None,
    }
}

fn type_spec_to_info(ts: &TypeSpec, st: &SymbolTable) -> TypeInfo {
    match ts {
        TypeSpec::Integer(sel) => TypeInfo::Integer { kind: extract_kind(sel, st) },
        TypeSpec::Real(sel) => TypeInfo::Real { kind: extract_kind(sel, st) },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(sel) => TypeInfo::Complex { kind: extract_kind(sel, st) },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(sel) => TypeInfo::Logical { kind: extract_kind(sel, st) },
        TypeSpec::Character(sel) => TypeInfo::Character { len: extract_char_len(sel), kind: None },
        TypeSpec::Type(name) => TypeInfo::Derived(name.clone()),
        TypeSpec::Class(name) => TypeInfo::Class(name.clone()),
        TypeSpec::ClassStar => TypeInfo::ClassStar,
        TypeSpec::TypeStar => TypeInfo::TypeStar,
    }
}

fn attrs_to_symbol_attrs(attrs: &[Attribute], default_access: Access) -> SymbolAttrs {
    let mut sa = SymbolAttrs { access: default_access, ..SymbolAttrs::default() };
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

    // ---- Integration tests ----

    #[test]
    fn simple_program_declarations() {
        let st = resolve_source("program test\n  implicit none\n  integer :: x, y\n  real :: z\nend program\n");
        // Should have x, y, z defined.
        // Navigate to the program scope.
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(prog_scope.symbols.contains_key("x"));
        assert!(prog_scope.symbols.contains_key("y"));
        assert!(prog_scope.symbols.contains_key("z"));
    }

    #[test]
    fn implicit_none_enforced() {
        let st = resolve_source("program test\n  implicit none\n  integer :: x\nend program\n");
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(prog_scope.implicit_rules.none_type);
    }

    #[test]
    fn module_use_association() {
        let st = resolve_source("\
module mymod
  implicit none
  integer :: shared_var
end module

program main
  use mymod
  implicit none
end program
");
        // shared_var should be in the module scope.
        let mod_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Module(ref n) if n == "mymod")).unwrap();
        assert!(mod_scope.symbols.contains_key("shared_var"));

        // The program should have a USE association for shared_var.
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(!prog_scope.use_associations.is_empty());
    }

    #[test]
    fn subroutine_with_args() {
        let st = resolve_source("subroutine foo(x, y)\n  real :: x, y\nend subroutine\n");
        let sub_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Subroutine(ref n) if n == "foo")).unwrap();
        assert!(sub_scope.symbols.contains_key("x"));
        assert!(sub_scope.symbols.contains_key("y"));
    }

    #[test]
    fn function_result_variable() {
        let st = resolve_source("function square(x) result(y)\n  real :: x, y\n  y = x * x\nend function\n");
        let fn_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Function(ref n) if n == "square")).unwrap();
        assert!(fn_scope.symbols.contains_key("x"));
        assert!(fn_scope.symbols.contains_key("y"));
    }

    #[test]
    fn contains_creates_child_scope() {
        let st = resolve_source("\
program main
  implicit none
  integer :: x
contains
  subroutine inner()
    integer :: local_var
  end subroutine
end program
");
        // inner should be its own scope.
        let inner_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Subroutine(ref n) if n == "inner")).unwrap();
        assert!(inner_scope.symbols.contains_key("local_var"));

        // inner should be registered as a symbol in the program scope.
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(prog_scope.symbols.contains_key("inner"));
    }

    #[test]
    fn derived_type_defined() {
        let st = resolve_source("module m\n  type :: mytype\n    integer :: field\n  end type\nend module\n");
        let mod_scope = st.scopes.iter().find(|s| matches!(&s.kind, ScopeKind::Module(n) if n == "m")).unwrap();
        assert!(mod_scope.symbols.contains_key("mytype"));
        assert_eq!(mod_scope.symbols["mytype"].kind, SymbolKind::DerivedType);
    }
}
