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
use std::collections::{BTreeSet, HashMap, HashSet};

use super::statement_functions::detect_statement_functions;
use super::type_resolution::{derived_char_init_len, entity_char_len_to_info, type_spec_to_info};
use super::use_resolution::{
    load_external_module, load_external_submodule, preload_stmt_uses, process_uses,
};

thread_local! {
    /// Track externally loaded module interfaces so resolve_file can
    /// return them to the driver for globals extraction.
    pub(super) static LOADED_EXTERNAL_MODULES: RefCell<Vec<crate::sema::amod::ModuleInterface>> = const { RefCell::new(Vec::new()) };
    pub(super) static LOADING_EXTERNAL_SUBMODULES: RefCell<HashSet<(String, String)>> = RefCell::new(HashSet::new());
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

fn directly_use_associated_generic_interfaces<'a>(
    st: &'a SymbolTable,
    scope_id: ScopeId,
    generic_name: &str,
) -> Vec<&'a Symbol> {
    let direct_uses: Vec<_> = st
        .scope(scope_id)
        .use_associations
        .iter()
        .filter(|association| !association.is_submodule_access)
        .cloned()
        .collect();
    st.named_interface_symbols_from_use_associations(&direct_uses, generic_name)
}

fn merged_visible_generic_specifics(
    st: &SymbolTable,
    scope_id: ScopeId,
    generic_name: &str,
    local_specifics: &[String],
) -> Vec<String> {
    let mut merged = Vec::new();
    if let Some(existing) = st.named_interface_facet_symbol_in_scope(scope_id, generic_name) {
        if existing.scope == scope_id {
            merge_specific_names(&mut merged, &existing.arg_names);
        }
    }
    for interface in directly_use_associated_generic_interfaces(st, scope_id, generic_name) {
        merge_specific_names(&mut merged, &interface.arg_names);
    }
    merge_specific_names(&mut merged, local_specifics);
    merged
}

fn interface_specific_names(bodies: &[InterfaceBody]) -> Vec<String> {
    let mut names = Vec::new();
    for body in bodies {
        match body {
            InterfaceBody::Subprogram(sub) => match &sub.node {
                ProgramUnit::Function { name, .. } | ProgramUnit::Subroutine { name, .. } => {
                    names.push(name.to_lowercase());
                }
                _ => {}
            },
            InterfaceBody::ModuleProcedure(procedures) => {
                names.extend(procedures.iter().map(|name| name.to_lowercase()));
            }
        }
    }
    names
}

fn procedure_owner_scope(st: &SymbolTable, from_scope: ScopeId, name: &str) -> ScopeId {
    st.lookup_in(from_scope, name)
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
        .unwrap_or(from_scope)
}

fn duplicate_generic_specific_error(
    generic_name: &str,
    specific: &str,
    span: crate::lexer::Span,
) -> SemaError {
    SemaError {
        span,
        msg: format!(
            "specific procedure '{specific}' is already present in generic interface \
             '{generic_name}'"
        ),
    }
}

fn validate_explicit_generic_specifics(
    st: &SymbolTable,
    scope_id: ScopeId,
    generic_name: &str,
    local_specifics: &[String],
    span: crate::lexer::Span,
) -> Result<(), SemaError> {
    let mut declared_here = HashSet::new();
    let mut visible_specifics = HashSet::new();
    let interfaces = directly_use_associated_generic_interfaces(st, scope_id, generic_name);
    let mut seen_interfaces = HashSet::new();
    for interface in interfaces {
        let interface_key = (interface.scope, interface.name.to_ascii_lowercase());
        if interface.scope == scope_id || !seen_interfaces.insert(interface_key) {
            continue;
        }
        visible_specifics.extend(interface.arg_names.iter().map(|specific| {
            (
                specific.to_ascii_lowercase(),
                procedure_owner_scope(st, interface.scope, specific),
            )
        }));
    }

    for specific in local_specifics {
        let key = specific.to_ascii_lowercase();
        if !declared_here.insert(key.clone()) {
            return Err(duplicate_generic_specific_error(
                generic_name,
                specific,
                span,
            ));
        }

        // A specific may have the same local name as a different procedure
        // owned by another module. Compare owner scope as well as spelling so
        // legal generic merges retain that distinction.
        let local_owner = procedure_owner_scope(st, scope_id, specific);
        if visible_specifics.contains(&(key, local_owner)) {
            return Err(duplicate_generic_specific_error(
                generic_name,
                specific,
                span,
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ProcedureNature {
    Function,
    Subroutine,
}

#[derive(Clone, Copy, Default)]
struct GenericProcedureNatures {
    saw_function: bool,
    saw_subroutine: bool,
}

impl GenericProcedureNatures {
    fn add(&mut self, nature: ProcedureNature) {
        match nature {
            ProcedureNature::Function => self.saw_function = true,
            ProcedureNature::Subroutine => self.saw_subroutine = true,
        }
    }

    fn is_mixed(self) -> bool {
        self.saw_function && self.saw_subroutine
    }
}

fn procedure_nature(symbol: &Symbol) -> Option<ProcedureNature> {
    match symbol.kind {
        SymbolKind::Function => Some(ProcedureNature::Function),
        SymbolKind::Subroutine => Some(ProcedureNature::Subroutine),
        SymbolKind::ExternalProc if symbol.type_info.is_some() => Some(ProcedureNature::Function),
        _ => None,
    }
}

fn generic_procedure_natures_from_interfaces<'a>(
    st: &SymbolTable,
    interfaces: impl IntoIterator<Item = &'a Symbol>,
) -> GenericProcedureNatures {
    let mut natures = GenericProcedureNatures::default();
    let mut seen_specifics = HashSet::new();
    for interface in interfaces {
        for specific in &interface.arg_names {
            let Some(symbol) = st.lookup_in(interface.scope, specific) else {
                continue;
            };
            let Some(nature) = procedure_nature(symbol) else {
                continue;
            };
            if seen_specifics.insert((symbol.scope, symbol.name.to_ascii_lowercase(), nature)) {
                natures.add(nature);
            }
        }
    }
    natures
}

fn visible_generic_procedure_natures(
    st: &SymbolTable,
    scope_id: ScopeId,
    generic_name: &str,
) -> GenericProcedureNatures {
    let scope = st.scope(scope_id);
    let mut interfaces = st.named_interface_symbols_in(scope_id, generic_name);
    interfaces.extend(
        st.named_interface_symbols_from_use_associations(&scope.use_associations, generic_name),
    );
    generic_procedure_natures_from_interfaces(st, interfaces)
}

fn use_associated_generic_procedure_natures(
    st: &SymbolTable,
    scope_id: ScopeId,
    generic_name: &str,
) -> GenericProcedureNatures {
    let interfaces = directly_use_associated_generic_interfaces(st, scope_id, generic_name);
    generic_procedure_natures_from_interfaces(st, interfaces)
}

fn mixed_generic_procedure_natures_error(
    generic_name: &str,
    span: crate::lexer::Span,
) -> SemaError {
    SemaError {
        span,
        msg: format!(
            "generic interface '{}' may not mix function and subroutine specific procedures",
            generic_name.to_ascii_lowercase()
        ),
    }
}

fn declared_procedure_nature(
    st: &SymbolTable,
    scope_id: ScopeId,
    unit: &ProgramUnit,
) -> Option<(String, ProcedureNature)> {
    match unit {
        ProgramUnit::Function { name, .. } => {
            Some((name.to_ascii_lowercase(), ProcedureNature::Function))
        }
        ProgramUnit::Subroutine {
            name, args, prefix, ..
        } => {
            let inherited_nature = (args.is_empty()
                && prefix.iter().any(|item| matches!(item, Prefix::Module))
                && matches!(st.scope(scope_id).kind, ScopeKind::Submodule(_)))
            .then(|| st.lookup_in(scope_id, name))
            .flatten()
            .and_then(procedure_nature);
            Some((
                name.to_ascii_lowercase(),
                inherited_nature.unwrap_or(ProcedureNature::Subroutine),
            ))
        }
        _ => None,
    }
}

fn declared_procedure_natures(
    st: &SymbolTable,
    scope_id: ScopeId,
    units: &[SpannedUnit],
) -> HashMap<String, ProcedureNature> {
    let mut natures = HashMap::new();
    for unit in units {
        if let Some((name, nature)) = declared_procedure_nature(st, scope_id, &unit.node) {
            natures.entry(name).or_insert(nature);
        }
        let ProgramUnit::InterfaceBlock { bodies, .. } = &unit.node else {
            continue;
        };
        for body in bodies {
            let InterfaceBody::Subprogram(subprogram) = body else {
                continue;
            };
            if let Some((name, nature)) = declared_procedure_nature(st, scope_id, &subprogram.node)
            {
                natures.entry(name).or_insert(nature);
            }
        }
    }
    natures
}

fn validate_local_generic_declarations(
    st: &SymbolTable,
    units: &[SpannedUnit],
    containing_span: crate::lexer::Span,
) -> Result<(), SemaError> {
    let scope_id = st.current_scope();
    let local_procedure_natures = declared_procedure_natures(st, scope_id, units);
    let mut generic_natures = HashMap::new();
    let mut visible_generic_names = BTreeSet::new();
    for symbol in st.scope(scope_id).symbols.values() {
        if symbol.kind == SymbolKind::NamedInterface {
            visible_generic_names.insert(symbol.name.to_ascii_lowercase());
        }
    }
    for association in &st.scope(scope_id).use_associations {
        if !association.local_name.is_empty() {
            visible_generic_names.insert(association.local_name.to_ascii_lowercase());
        }
    }
    for generic_name in visible_generic_names {
        let natures = visible_generic_procedure_natures(st, scope_id, &generic_name);
        if natures.is_mixed() {
            return Err(mixed_generic_procedure_natures_error(
                &generic_name,
                containing_span,
            ));
        }
        generic_natures.insert(generic_name, natures);
    }

    let mut declared = HashSet::new();
    for unit in units {
        let ProgramUnit::InterfaceBlock {
            name: Some(generic_name),
            bodies,
            ..
        } = &unit.node
        else {
            continue;
        };
        if generic_name.is_empty() {
            continue;
        }
        let specifics = interface_specific_names(bodies);
        validate_explicit_generic_specifics(st, scope_id, generic_name, &specifics, unit.span)?;
        let generic_key = generic_name.to_ascii_lowercase();
        let natures = generic_natures
            .entry(generic_key.clone())
            .or_insert_with(|| {
                use_associated_generic_procedure_natures(st, scope_id, &generic_key)
            });
        for body in bodies {
            match body {
                InterfaceBody::Subprogram(subprogram) => {
                    if let Some((_, nature)) =
                        declared_procedure_nature(st, scope_id, &subprogram.node)
                    {
                        natures.add(nature);
                    }
                }
                InterfaceBody::ModuleProcedure(procedures) => {
                    for procedure in procedures {
                        let key = procedure.to_ascii_lowercase();
                        let nature = local_procedure_natures
                            .get(&key)
                            .copied()
                            .or_else(|| st.lookup_in(scope_id, &key).and_then(procedure_nature));
                        if let Some(nature) = nature {
                            natures.add(nature);
                        }
                    }
                }
            }
            if natures.is_mixed() {
                return Err(mixed_generic_procedure_natures_error(
                    generic_name,
                    unit.span,
                ));
            }
        }
        for specific in specifics {
            let specific_key = specific.to_ascii_lowercase();
            if !declared.insert((generic_key.clone(), specific_key)) {
                return Err(duplicate_generic_specific_error(
                    generic_name,
                    &specific,
                    unit.span,
                ));
            }
        }
    }
    Ok(())
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

    // Register intrinsic modules so USE can find them.
    crate::sema::intrinsic_modules::register_intrinsic_modules(&mut st);

    // First pass: create module scopes so USE can find them.
    let mut module_definitions = HashMap::new();
    for unit in units {
        if let ProgramUnit::Module { name, .. } = &unit.node {
            let key = name.to_ascii_lowercase();
            if let Some(first_span) = module_definitions.insert(key, unit.span) {
                return Err(SemaError {
                    span: unit.span,
                    msg: format!(
                        "duplicate module program unit '{}' (first defined at {}:{})",
                        name, first_span.start.line, first_span.start.col
                    ),
                });
            }
            st.push_scope(ScopeKind::Module(name.clone()));
            st.pop_scope();
        }
    }

    // Second pass: populate all scopes (loads .amod files lazily on USE miss).
    // Track which external modules were loaded.
    LOADED_EXTERNAL_MODULES.with(|cell| cell.borrow_mut().clear());
    LOADING_EXTERNAL_SUBMODULES.with(|cell| cell.borrow_mut().clear());
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
    struct InterfaceUpdate {
        key: String,
        type_info: Option<TypeInfo>,
        arg_names: Vec<String>,
        result_rank: u8,
        result_array_spec: Vec<decl::ArraySpec>,
    }

    let updates: Vec<InterfaceUpdate> = st
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
            Some(InterfaceUpdate {
                key: key.clone(),
                type_info: iface_sym.type_info.clone(),
                arg_names: iface_sym.arg_names.clone(),
                result_rank: iface_sym.attrs.result_rank,
                result_array_spec: iface_sym.attrs.array_spec.clone(),
            })
        })
        .collect();

    for update in updates {
        if let Some(sym) = st.scope_mut(scope_id).symbols.get_mut(&update.key) {
            if let Some(type_info) = update.type_info {
                sym.type_info = Some(type_info);
            }
            sym.arg_names = update.arg_names;
            sym.attrs.result_rank = update.result_rank;
            sym.attrs.array_spec = update.result_array_spec;
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ProcedureBinding {
    bind_c: bool,
    label: Option<String>,
}

fn resolved_procedure_binding(
    bind: Option<&crate::ast::unit::BindInfo>,
    default_name: &str,
    st: &SymbolTable,
    scope_id: ScopeId,
) -> Result<ProcedureBinding, SemaError> {
    let Some(bind) = bind else {
        return Ok(ProcedureBinding::default());
    };
    let Some(expr) = &bind.name else {
        return Ok(ProcedureBinding {
            bind_c: true,
            label: Some(default_name.to_string()),
        });
    };
    if !matches!(
        crate::sema::types::expr_type(expr, st),
        crate::sema::types::FortranType::Character { kind: 1, .. }
    ) {
        return Err(SemaError {
            span: expr.span,
            msg: "BIND(C) NAME= must be a scalar default-character constant expression".into(),
        });
    }
    let value = eval_const_char_expr_in_scope(expr, st, scope_id).ok_or_else(|| SemaError {
        span: expr.span,
        msg: "BIND(C) NAME= must be a scalar default-character constant expression".into(),
    })?;
    if value.is_empty() {
        return Ok(ProcedureBinding {
            bind_c: true,
            label: None,
        });
    }
    let mut chars = value.chars();
    let valid = chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if !valid {
        return Err(SemaError {
            span: expr.span,
            msg: "BIND(C) NAME= must evaluate to a valid C identifier or an empty string".into(),
        });
    }
    Ok(ProcedureBinding {
        bind_c: true,
        label: Some(value),
    })
}

fn unresolved_procedure_binding(
    bind: Option<&crate::ast::unit::BindInfo>,
    default_name: &str,
) -> ProcedureBinding {
    ProcedureBinding {
        bind_c: bind.is_some(),
        label: bind.map(|_| default_name.to_string()),
    }
}

struct InterfaceOuterRef {
    name: String,
    kind: SymbolKind,
    type_info: Option<TypeInfo>,
    arg_names: Vec<String>,
    bind_c: bool,
    binding_label: Option<String>,
    pure: bool,
    elemental: bool,
    abstract_interface: bool,
    result_attrs: SymbolAttrs,
    defined_at: crate::lexer::Span,
}

fn can_merge_interface_body_with_dummy(symbol: &Symbol) -> bool {
    // Header dummies begin as variables. Declarations that establish
    // incompatible data-object or procedure attributes make the placeholder
    // ineligible to become the explicitly declared procedure.
    symbol.kind == SymbolKind::Variable
        && symbol.attrs.array_spec.is_empty()
        && !symbol.attrs.allocatable
        && !symbol.attrs.intrinsic
        && !symbol.attrs.save
        && !symbol.attrs.target
        && !symbol.attrs.value
        && !(symbol.attrs.external && symbol.type_info.is_some())
}

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
            process_contains(st, contains, module_search_paths, layouts, unit.span)?;
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
            if let Some(mod_id) = st.find_non_intrinsic_module_scope(name) {
                let saved = st.enter_scope(mod_id);

                process_uses(st, uses, module_search_paths, layouts)?;
                process_implicit(st, implicit)?;
                process_decls(st, decls)?;
                process_contains(st, contains, module_search_paths, layouts, unit.span)?;
                backfill_procedure_pointer_interfaces(st, mod_id);

                st.enter_scope(saved);
            }
        }
        ProgramUnit::Subroutine {
            name,
            args,
            prefix,
            bind,
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
            let binding = if is_separate_body && bind.is_none() {
                ProcedureBinding {
                    bind_c: st.scope(scope_id).bind_c,
                    label: st.scope(scope_id).binding_label.clone(),
                }
            } else {
                resolved_procedure_binding(bind.as_ref(), name, st, scope_id)?
            };
            st.scope_mut(scope_id).bind_c = binding.bind_c;
            st.scope_mut(scope_id).binding_label = binding.label;
            process_namelists(st, body)?;
            detect_statement_functions(st, scope_id, body);
            preload_stmt_uses(st, body, module_search_paths, layouts);
            process_contains(st, contains, module_search_paths, layouts, unit.span)?;
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::Function {
            name,
            args,
            result,
            return_type,
            bind,
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
            st.scope_mut(scope_id).result_name = Some(result_name.to_string());
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
            let binding = resolved_procedure_binding(bind.as_ref(), name, st, scope_id)?;
            st.scope_mut(scope_id).bind_c = binding.bind_c;
            st.scope_mut(scope_id).binding_label = binding.label;
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
            process_contains(st, contains, module_search_paths, layouts, unit.span)?;
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::BlockData { name, uses, decls } => {
            let scope_name = name.clone().unwrap_or_else(|| "<block_data>".into());
            st.push_scope(ScopeKind::Program(scope_name));
            process_uses(st, uses, module_search_paths, layouts)?;
            process_decls(st, decls)?;
            validate_local_generic_declarations(st, &[], unit.span)?;
            st.pop_scope();
        }
        ProgramUnit::Submodule {
            parent,
            ancestor,
            name,
            uses,
            imports,
            implicit,
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
            // Resolve the exact immediate semantic parent. Descendants load
            // the parent submodule's .smod identity record and AMOD companion;
            // the ancestor module is not a substitute for that scope.
            let parent_scope = if let Some(immediate_parent) = ancestor {
                st.find_submodule_scope(parent, immediate_parent)
                    .or_else(|| {
                        load_external_submodule(
                            st,
                            parent,
                            immediate_parent,
                            module_search_paths,
                            layouts,
                        )
                    })
            } else {
                st.find_module_scope(parent)
                    .or_else(|| load_external_module(st, parent, module_search_paths, layouts))
            }
            .ok_or_else(|| SemaError {
                span: unit.span,
                msg: if let Some(immediate_parent) = ancestor {
                    format!(
                        "immediate parent submodule '{}:{}' was not found",
                        parent, immediate_parent
                    )
                } else {
                    format!("parent module '{}' was not found", parent)
                },
            })?;
            let scope_id = st.push_scope(ScopeKind::Submodule(name.clone()));
            st.set_submodule_ancestor(scope_id, parent);
            process_imports(
                st,
                scope_id,
                imports,
                HostAssociationPolicy::All,
                unit.span,
                Some(parent_scope),
            )?;
            process_uses(st, uses, module_search_paths, layouts)?;
            process_implicit(st, implicit)?;
            // Import all immediate-parent symbols into the submodule scope.
            // Per F2008 12.2.3.2: submodules see ALL parent entities,
            // including private ones — that's the whole point of the
            // submodule mechanism (host association).
            st.add_use_association(UseAssociation {
                local_name: String::new(),
                original_name: String::new(),
                source_scope: parent_scope,
                is_submodule_access: true,
                from_bare_use: true,
            });
            let parent_syms: Vec<(String, String)> = st
                .scope(parent_scope)
                .symbols
                .iter()
                .map(|(key, sym)| (sym.name.clone(), key.clone()))
                .collect();
            for (sym_name, _key) in &parent_syms {
                st.add_use_association(UseAssociation {
                    local_name: sym_name.clone(),
                    original_name: sym_name.clone(),
                    source_scope: parent_scope,
                    is_submodule_access: true,
                    from_bare_use: true,
                });
            }
            process_decls(st, decls)?;
            process_contains(st, contains, module_search_paths, layouts, unit.span)?;
            backfill_procedure_pointer_interfaces(st, scope_id);
            st.pop_scope();
        }
        ProgramUnit::InterfaceBlock {
            name,
            is_abstract,
            bodies,
        } => {
            let specific_names = interface_specific_names(bodies);
            if let Some(generic_name) = name.as_deref().filter(|name| !name.is_empty()) {
                validate_explicit_generic_specifics(
                    st,
                    st.current_scope(),
                    generic_name,
                    &specific_names,
                    unit.span,
                )?;
            }

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
                            let mut result_attrs_for_iface =
                                function_result_attrs(fn_name, result, decls);
                            result_attrs_for_iface.is_separate_module_interface = prefix
                                .iter()
                                .any(|item| matches!(item, crate::ast::unit::Prefix::Module));
                            let binding = unresolved_procedure_binding(bind.as_ref(), fn_name);
                            outer_refs.push(InterfaceOuterRef {
                                name: fn_name.clone(),
                                kind: SymbolKind::Function,
                                type_info: ti,
                                arg_names,
                                bind_c: binding.bind_c,
                                binding_label: binding.label,
                                pure,
                                elemental,
                                abstract_interface: *is_abstract,
                                result_attrs: result_attrs_for_iface,
                                defined_at: sub.span,
                            });
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
                            let is_separate_module_interface = prefix
                                .iter()
                                .any(|item| matches!(item, crate::ast::unit::Prefix::Module));
                            let binding = unresolved_procedure_binding(bind.as_ref(), fn_name);
                            outer_refs.push(InterfaceOuterRef {
                                name: fn_name.clone(),
                                kind: SymbolKind::Subroutine,
                                type_info: None,
                                arg_names,
                                bind_c: binding.bind_c,
                                binding_label: binding.label,
                                pure,
                                elemental,
                                abstract_interface: *is_abstract,
                                result_attrs: SymbolAttrs {
                                    is_separate_module_interface,
                                    ..Default::default()
                                },
                                defined_at: sub.span,
                            });
                        }
                        _ => {}
                    }
                }
            }

            let interface_scope = st.push_scope(ScopeKind::Interface);
            for body in bodies {
                match body {
                    InterfaceBody::Subprogram(sub) => {
                        resolve_unit(st, sub, module_search_paths, layouts)?;
                        if let Some(proc_scope) = find_unit_scope(st, interface_scope, &sub.node) {
                            let bind_c = st.scope(proc_scope).bind_c;
                            let binding_label = st.scope(proc_scope).binding_label.clone();
                            let (name, kind) = match &sub.node {
                                ProgramUnit::Function { name, .. } => (name, SymbolKind::Function),
                                ProgramUnit::Subroutine { name, .. } => {
                                    (name, SymbolKind::Subroutine)
                                }
                                _ => continue,
                            };
                            if let Some(outer_ref) = outer_refs.iter_mut().find(|outer_ref| {
                                outer_ref.name.eq_ignore_ascii_case(name) && outer_ref.kind == kind
                            }) {
                                outer_ref.bind_c = bind_c;
                                outer_ref.binding_label = binding_label;
                            }
                        }
                    }
                    InterfaceBody::ModuleProcedure(_) => {}
                }
            }
            st.pop_scope();

            // Surface each declared procedure to the enclosing scope
            // so callers under IMPLICIT NONE can resolve the name,
            // and so BIND(C) external prototypes are callable.
            for outer_ref in outer_refs {
                let InterfaceOuterRef {
                    name: fn_name,
                    kind,
                    type_info: ti,
                    arg_names,
                    bind_c,
                    binding_label,
                    pure,
                    elemental,
                    abstract_interface,
                    result_attrs,
                    defined_at,
                } = outer_ref;
                let symbol = Symbol {
                    name: fn_name.clone(),
                    kind: kind.clone(),
                    type_info: ti.clone(),
                    attrs: SymbolAttrs {
                        external: true,
                        bind_c,
                        binding_label: binding_label.clone(),
                        pure,
                        elemental,
                        abstract_interface,
                        allocatable: result_attrs.allocatable,
                        pointer: result_attrs.pointer,
                        result_rank: result_attrs.result_rank,
                        array_spec: result_attrs.array_spec.clone(),
                        is_separate_module_interface: result_attrs.is_separate_module_interface,
                        ..Default::default()
                    },
                    defined_at,
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
                    let mut merged_dummy = false;
                    if is_dummy_arg {
                        if let Some(existing) = st.scope_mut(scope_id).symbols.get_mut(&key) {
                            if can_merge_interface_body_with_dummy(existing) {
                                let mut attrs = existing.attrs.clone();
                                let dummy_is_pointer = attrs.pointer;
                                attrs.external = true;
                                attrs.bind_c = bind_c;
                                attrs.binding_label = binding_label;
                                attrs.pure = pure;
                                attrs.elemental = elemental;
                                attrs.allocatable = result_attrs.allocatable;
                                attrs.pointer = dummy_is_pointer || result_attrs.pointer;
                                attrs.result_rank = result_attrs.result_rank;
                                attrs.array_spec = result_attrs.array_spec;
                                attrs.is_separate_module_interface =
                                    result_attrs.is_separate_module_interface;
                                existing.kind = kind;
                                existing.type_info = ti;
                                existing.attrs = attrs;
                                existing.arg_names = arg_names;
                                merged_dummy = true;
                            }
                        }
                    }
                    if !merged_dummy {
                        return Err(err);
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
    let Some(interface_owner_scope) =
        st.find_separate_module_interface_scope(parent_module_scope, proc_name)
    else {
        return;
    };

    // Find the matching procedure scope inside the parent module.
    // F2008-style submodules declare the procedure inside an explicit
    // `interface ... end interface` block at module scope, which adds
    // an intermediate Interface scope between the module and the
    // procedure scope. Tolerate one Interface hop.
    let proc_lc = proc_name.to_lowercase();
    let iface_scope = st.all_scopes().iter().find_map(|scope| {
        let direct_parent_matches = scope.parent == Some(interface_owner_scope);
        let via_interface = scope
            .parent
            .map(|pid| {
                matches!(st.scope(pid).kind, ScopeKind::Interface)
                    && st.scope(pid).parent == Some(interface_owner_scope)
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
    let inherited_binding = if st.scope(iface_scope).bind_c {
        ProcedureBinding {
            bind_c: true,
            label: st.scope(iface_scope).binding_label.clone(),
        }
    } else {
        st.scope(parent_module_scope)
            .symbols
            .get(&proc_lc)
            .map(|symbol| ProcedureBinding {
                bind_c: symbol.attrs.bind_c,
                label: symbol.attrs.binding_label.clone(),
            })
            .unwrap_or_default()
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

    // Also clone the function result into the separate body. The interface
    // scope records its exact RESULT identity; unrelated locals and named
    // constants must never participate in this selection.
    let result_name = st.scope(iface_scope).result_name.clone();
    let result_sym = st.scope(iface_scope).procedure_result_symbol().cloned();

    st.scope_mut(body_scope).arg_order = arg_order;
    st.scope_mut(body_scope).bind_c = inherited_binding.bind_c;
    st.scope_mut(body_scope).binding_label = inherited_binding.label;
    st.scope_mut(body_scope).result_name = result_name.clone();
    for mut sym in arg_symbols {
        sym.scope = body_scope;
        sym.defined_at = span;
        let _ = st.define(sym);
    }
    if let Some(mut sym) = result_sym {
        sym.scope = body_scope;
        sym.defined_at = span;
        if let Some(result_name) = result_name {
            sym.name = result_name;
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
        Expr::StringLiteral { value, .. } => Some(value.source_view().into_owned()),
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
        Expr::StringLiteral { value, .. } => Some(value.source_view().into_owned()),
        Expr::Name { name } => {
            let sym = st.lookup_in(scope_id, &name.to_lowercase())?;
            if sym.attrs.parameter && sym.attrs.array_spec.is_empty() {
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

    crate::sema::specific_intrinsic::specific_intrinsic_wrapper_symbol(&key)
        .unwrap_or(target)
        .to_string()
}

fn mangle_link_symbol_for(
    sym: &crate::sema::symtab::Symbol,
    scope: &crate::sema::symtab::Scope,
    name_in_scope: &str,
) -> String {
    use crate::sema::symtab::{ScopeKind, SymbolKind};
    if sym.kind == SymbolKind::IntrinsicProc || sym.attrs.intrinsic {
        return crate::sema::specific_intrinsic::specific_intrinsic_wrapper_symbol(name_in_scope)
            .unwrap_or(name_in_scope)
            .to_string();
    }
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

fn implicit_type_to_type_info(implicit_type: ImplicitType) -> TypeInfo {
    match implicit_type {
        ImplicitType::Integer => TypeInfo::Integer { kind: None },
        ImplicitType::Real => TypeInfo::Real { kind: None },
        ImplicitType::DoublePrecision => TypeInfo::DoublePrecision,
        ImplicitType::Complex => TypeInfo::Complex { kind: None },
        ImplicitType::Logical => TypeInfo::Logical { kind: None },
        ImplicitType::Character => TypeInfo::Character {
            len: Some(1),
            kind: None,
        },
    }
}

fn is_unresolved_declaration_placeholder(
    st: &SymbolTable,
    name: &str,
    declared_kind: &SymbolKind,
) -> bool {
    if !matches!(
        declared_kind,
        SymbolKind::Variable | SymbolKind::ProcedurePointer
    ) {
        return false;
    }
    is_procedure_entity_placeholder(st, name)
        && st
            .scope(st.current_scope())
            .symbols
            .get(&name.to_ascii_lowercase())
            .is_some_and(|symbol| symbol.type_info.is_none())
}

fn is_procedure_entity_placeholder(st: &SymbolTable, name: &str) -> bool {
    let key = name.to_ascii_lowercase();
    let scope = st.scope(st.current_scope());
    let has_placeholder_identity = scope
        .arg_order
        .iter()
        .any(|argument| argument.eq_ignore_ascii_case(&key))
        || scope
            .result_name
            .as_deref()
            .is_some_and(|result| result.eq_ignore_ascii_case(&key));
    has_placeholder_identity
        && scope
            .symbols
            .get(&key)
            .is_some_and(|symbol| symbol.kind == SymbolKind::Variable)
}

fn repeated_symbol_access_error(name: &str, span: crate::lexer::Span) -> SemaError {
    SemaError {
        span,
        msg: format!(
            "accessibility of '{}' is specified more than once in this scope",
            name.to_ascii_lowercase()
        ),
    }
}

fn repeated_default_access_error(span: crate::lexer::Span) -> SemaError {
    SemaError {
        span,
        msg: "default accessibility is specified more than once in this scope".into(),
    }
}

fn entity_declaration_access(attrs: &[Attribute]) -> (Option<Access>, bool) {
    let mut access = None;
    for attr in attrs {
        let next = match attr {
            Attribute::Public => Access::Public,
            Attribute::Private => Access::Private,
            _ => continue,
        };
        if access.replace(next).is_some() {
            return (access, true);
        }
    }
    (access, false)
}

fn derived_type_declaration_access(attrs: &[decl::TypeAttr]) -> (Option<Access>, bool) {
    let mut access = None;
    for attr in attrs {
        let next = match attr {
            decl::TypeAttr::Public => Access::Public,
            decl::TypeAttr::Private => Access::Private,
            _ => continue,
        };
        if access.replace(next).is_some() {
            return (access, true);
        }
    }
    (access, false)
}

fn record_explicit_symbol_access(
    seen: &mut HashSet<String>,
    name: &str,
    span: crate::lexer::Span,
) -> Result<(), SemaError> {
    let key = name.to_ascii_lowercase();
    if !seen.insert(key) {
        return Err(repeated_symbol_access_error(name, span));
    }
    Ok(())
}

fn validate_accessibility_specifications(decls: &[SpannedDecl]) -> Result<(), SemaError> {
    let mut default_access_seen = false;
    let mut explicitly_accessible_names = HashSet::new();

    for declaration in decls {
        match &declaration.node {
            Decl::AccessDefault { .. } => {
                if default_access_seen {
                    return Err(repeated_default_access_error(declaration.span));
                }
                default_access_seen = true;
            }
            Decl::AccessList { names, .. } => {
                for name in names {
                    record_explicit_symbol_access(
                        &mut explicitly_accessible_names,
                        name,
                        declaration.span,
                    )?;
                }
            }
            Decl::TypeDecl {
                attrs, entities, ..
            } => {
                let (access, repeated) = entity_declaration_access(attrs);
                let Some(first_entity) = entities.first() else {
                    continue;
                };
                if repeated {
                    return Err(repeated_symbol_access_error(
                        &first_entity.name,
                        declaration.span,
                    ));
                }
                if access.is_some() {
                    for entity in entities {
                        record_explicit_symbol_access(
                            &mut explicitly_accessible_names,
                            &entity.name,
                            declaration.span,
                        )?;
                    }
                }
            }
            Decl::DerivedTypeDef { name, attrs, .. } => {
                let (access, repeated) = derived_type_declaration_access(attrs);
                if repeated {
                    return Err(repeated_symbol_access_error(name, declaration.span));
                }
                if access.is_some() {
                    record_explicit_symbol_access(
                        &mut explicitly_accessible_names,
                        name,
                        declaration.span,
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn process_decls(st: &mut SymbolTable, decls: &[SpannedDecl]) -> Result<(), SemaError> {
    validate_accessibility_specifications(decls)?;

    // Collect AccessList entries — they must be applied AFTER all TypeDecls
    // because the list may reference symbols declared later in the module.
    let mut pending_access: Vec<(Access, Vec<String>, crate::lexer::Span)> = Vec::new();
    for decl in decls {
        match &decl.node {
            Decl::AccessDefault { access } => {
                let access = match access {
                    Attribute::Private => Access::Private,
                    Attribute::Public => Access::Public,
                    _ => continue,
                };
                if !st.set_default_access(access) {
                    return Err(repeated_default_access_error(decl.span));
                }
            }
            Decl::AccessList { access, names } => {
                let acc = match access {
                    Attribute::Private => Access::Private,
                    Attribute::Public => Access::Public,
                    _ => continue,
                };
                pending_access.push((acc, names.clone(), decl.span));
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
                            sym_attrs.result_rank = iface_sym.attrs.result_rank;
                            sym_attrs.array_spec = iface_sym.attrs.array_spec.clone();
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
                                derived_char_init_len(&init.node, st, st.current_scope())
                                    .map(|n| n as i64);
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
                    if is_unresolved_declaration_placeholder(st, &key, &kind) {
                        // Dummy arguments and untyped function results are
                        // registered before their declaration part. Complete
                        // each placeholder exactly once; every other local
                        // symbol must pass through `define` so collisions are
                        // diagnosed instead of mutating the earlier entity.
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
            Decl::DimensionStmt { entities } => {
                for entity in entities {
                    let key = entity.name.to_ascii_lowercase();
                    let is_placeholder = is_procedure_entity_placeholder(st, &key);
                    if is_placeholder {
                        let existing_type = st
                            .scope(st.current_scope())
                            .symbols
                            .get(&key)
                            .and_then(|symbol| symbol.type_info.clone());
                        let type_info = match existing_type {
                            Some(type_info) => type_info,
                            None => st
                                .implicit_type(&entity.name)
                                .map(implicit_type_to_type_info)
                                .ok_or_else(|| SemaError {
                                    span: decl.span,
                                    msg: format!(
                                        "array '{}' in DIMENSION statement has no implicit type",
                                        entity.name
                                    ),
                                })?,
                        };
                        let current_scope = st.current_scope();
                        let symbol = st
                            .scope_mut(current_scope)
                            .symbols
                            .get_mut(&key)
                            .expect("declaration placeholder should remain in its scope");
                        if !symbol.attrs.array_spec.is_empty() {
                            return Err(SemaError {
                                span: decl.span,
                                msg: format!(
                                    "duplicate DIMENSION attribute specified for '{}'",
                                    entity.name
                                ),
                            });
                        }
                        symbol.type_info = Some(type_info);
                        symbol.attrs.array_spec = entity.array_spec.clone();
                    } else {
                        let type_info = st
                            .implicit_type(&entity.name)
                            .map(implicit_type_to_type_info)
                            .ok_or_else(|| SemaError {
                                span: decl.span,
                                msg: format!(
                                    "array '{}' in DIMENSION statement has no implicit type",
                                    entity.name
                                ),
                            })?;
                        st.define(Symbol {
                            name: entity.name.clone(),
                            kind: SymbolKind::Variable,
                            type_info: Some(type_info),
                            attrs: SymbolAttrs {
                                access: st.default_access(st.current_scope()),
                                array_spec: entity.array_spec.clone(),
                                ..Default::default()
                            },
                            defined_at: decl.span,
                            scope: st.current_scope(),
                            arg_names: vec![],
                            const_value: None,
                            const_char_value: None,
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
    for (access, names, span) in &pending_access {
        for name in names {
            if !st.set_symbol_access(name, *access) {
                return Err(repeated_symbol_access_error(name, *span));
            }
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
    containing_span: crate::lexer::Span,
) -> Result<(), SemaError> {
    validate_local_generic_declarations(st, contains, containing_span)?;

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
                let binding = unresolved_procedure_binding(bind.as_ref(), name);
                let attrs = SymbolAttrs {
                    pure,
                    elemental,
                    bind_c: binding.bind_c,
                    binding_label: binding.label,
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
                st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Subroutine,
                    type_info: None,
                    attrs,
                    defined_at: unit.span,
                    scope: st.current_scope(),
                    arg_names,
                    const_value: None,
                    const_char_value: None,
                })?;
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
                let binding = unresolved_procedure_binding(bind.as_ref(), name);
                let fn_attrs = SymbolAttrs {
                    allocatable: result_attrs.allocatable,
                    pointer: result_attrs.pointer,
                    pure: fn_pure,
                    elemental: fn_elemental,
                    bind_c: binding.bind_c,
                    binding_label: binding.label,
                    result_rank: result_attrs.result_rank,
                    is_separate_module_procedure: fn_is_smp,
                    ..Default::default()
                };
                st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    type_info: ret_type_info,
                    attrs: fn_attrs,
                    defined_at: unit.span,
                    scope: st.current_scope(),
                    arg_names: vec![],
                    const_value: None,
                    const_char_value: None,
                })?;
            }
            _ => {}
        }
        let host_scope = st.current_scope();
        resolve_unit(st, unit, module_search_paths, layouts)?;
        if let Some(proc_scope) = find_unit_scope(st, host_scope, &unit.node) {
            let bind_c = st.scope(proc_scope).bind_c;
            let binding_label = st.scope(proc_scope).binding_label.clone();
            let name = match &unit.node {
                ProgramUnit::Subroutine { name, .. } | ProgramUnit::Function { name, .. } => name,
                _ => continue,
            };
            if let Some(symbol) = st
                .scope_mut(host_scope)
                .symbols
                .get_mut(&name.to_ascii_lowercase())
            {
                symbol.attrs.bind_c = bind_c;
                symbol.attrs.binding_label = binding_label;
            }
        }
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
            Attribute::Asynchronous => sa.asynchronous = true,
            Attribute::Contiguous => sa.contiguous = true,
            Attribute::Volatile => sa.volatile = true,
            Attribute::Target => sa.target = true,
            Attribute::Optional => sa.optional = true,
            Attribute::Save => sa.save = true,
            Attribute::Parameter => sa.parameter = true,
            Attribute::Value => sa.value = true,
            Attribute::Procedure => {}
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
        match &decl.node {
            crate::ast::decl::Decl::TypeDecl {
                attrs, entities, ..
            } => {
                let matching_entity = entities
                    .iter()
                    .find(|entity| entity.name.eq_ignore_ascii_case(&result_key));
                if let Some(entity) = matching_entity {
                    let mut sym_attrs = attrs_to_symbol_attrs(attrs, Access::Default);
                    // Preserve the full result shape: prefer the entity-local
                    // array spec (e.g. `real :: w(:)`), falling back to a
                    // `dimension(...)` attribute on the type declaration.
                    let array_spec = entity.array_spec.clone().or_else(|| {
                        attrs.iter().find_map(|a| match a {
                            crate::ast::decl::Attribute::Dimension(specs) => Some(specs.clone()),
                            _ => None,
                        })
                    });
                    sym_attrs.result_rank = array_spec
                        .as_ref()
                        .map(|specs| specs.len())
                        .unwrap_or(0)
                        .min(u8::MAX as usize) as u8;
                    sym_attrs.array_spec = array_spec.unwrap_or_default();
                    return sym_attrs;
                }
            }
            crate::ast::decl::Decl::DimensionStmt { entities } => {
                if let Some(entity) = entities
                    .iter()
                    .find(|entity| entity.name.eq_ignore_ascii_case(&result_key))
                {
                    return SymbolAttrs {
                        result_rank: entity.array_spec.len().min(u8::MAX as usize) as u8,
                        array_spec: entity.array_spec.clone(),
                        ..SymbolAttrs::default()
                    };
                }
            }
            _ => {}
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

    fn resolve_error(src: &str) -> SemaError {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        match resolve_file(&units, &[], crate::target::TargetLayout::LP64) {
            Ok(_) => panic!("expected semantic resolution to fail for source:\n{src}"),
            Err(err) => err,
        }
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
    fn function_scope_retains_explicit_result_identity() {
        let st = resolve_source(
            "\
module result_parent
  implicit none
  interface
    module function answer() result(actual_result)
      integer, parameter :: decoy = 101
      integer :: actual_result
    end function answer
  end interface
end module result_parent
",
        );
        let function_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Function(name) if name == "answer"))
            .unwrap();
        assert_eq!(function_scope.result_name.as_deref(), Some("actual_result"));
        let result = function_scope
            .procedure_result_symbol()
            .expect("explicit function result symbol was not retained");
        assert_eq!(result.name, "actual_result");
        assert!(!result.attrs.parameter);
        assert!(function_scope.symbols.contains_key("decoy"));
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
    fn standalone_dimension_defines_an_implicitly_typed_array() {
        let st = resolve_source(
            "program test\n\
               dimension :: x(-1:1, 4)\n\
             end program test\n",
        );
        let program_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(scope.kind, ScopeKind::Program(_)))
            .unwrap();
        let symbol = program_scope
            .symbols
            .get("x")
            .expect("DIMENSION entity should be declared");
        assert_eq!(symbol.type_info, Some(TypeInfo::Real { kind: None }));
        assert_eq!(symbol.attrs.array_spec.len(), 2);
    }

    #[test]
    fn standalone_dimension_obeys_implicit_none() {
        let error = resolve_error(
            "program test\n\
               implicit none\n\
               dimension :: x(3)\n\
             end program test\n",
        );
        assert!(
            error.msg.contains("has no implicit type"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn standalone_dimension_can_shape_a_prefixed_function_result() {
        let st = resolve_source(
            "module result_shapes\n\
               implicit none\n\
             contains\n\
               real function values()\n\
                 implicit none\n\
                 dimension :: values(3)\n\
               end function values\n\
             end module result_shapes\n",
        );
        let function_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Function(name) if name == "values"))
            .unwrap();
        let result = function_scope
            .procedure_result_symbol()
            .expect("function result symbol");
        assert_eq!(result.type_info, Some(TypeInfo::Real { kind: None }));
        assert_eq!(result.attrs.array_spec.len(), 1);
        let module_scope = st
            .scopes
            .iter()
            .find(|scope| {
                matches!(
                    &scope.kind,
                    ScopeKind::Module(name) if name == "result_shapes"
                )
            })
            .expect("module scope");
        let function = module_scope
            .symbols
            .get("values")
            .expect("contained function symbol");
        assert_eq!(function.attrs.result_rank, 1);
    }

    #[test]
    fn procedure_pointer_inherits_array_result_shape_from_interface() {
        let st = resolve_source(
            "program array_factory_user\n\
               implicit none\n\
               abstract interface\n\
                 function array_factory(n) result(values)\n\
                   integer, intent(in) :: n\n\
                   integer, dimension(n) :: values\n\
                 end function array_factory\n\
               end interface\n\
               procedure(array_factory), pointer :: make_values\n\
             end program array_factory_user\n",
        );
        let program_scope = st
            .scopes
            .iter()
            .find(|scope| {
                matches!(
                    &scope.kind,
                    ScopeKind::Program(name) if name == "array_factory_user"
                )
            })
            .expect("program scope");
        let procedure_pointer = program_scope
            .symbols
            .get("make_values")
            .expect("procedure pointer symbol");

        assert_eq!(procedure_pointer.kind, SymbolKind::ProcedurePointer);
        assert_eq!(
            procedure_pointer.attrs.procedure_iface.as_deref(),
            Some("array_factory")
        );
        assert_eq!(procedure_pointer.attrs.result_rank, 1);
        assert_eq!(procedure_pointer.attrs.array_spec.len(), 1);
    }

    #[test]
    fn submodule_implicit_none_is_retained_in_scope() {
        let st = resolve_source(
            "module parent\nend module parent\nsubmodule(parent) child\n  implicit none\nend submodule child\n",
        );
        let submodule_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Submodule(name) if name == "child"))
            .unwrap();
        assert!(submodule_scope.implicit_rules.none_type);
        assert!(submodule_scope.has_explicit_implicit_stmt);
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
    fn intrinsic_use_rejects_non_intrinsic_module() {
        let err = resolve_error(
            "module user_module\nend module\nprogram p\n  use, intrinsic :: user_module\nend program\n",
        );
        assert!(
            err.msg.contains("not an intrinsic module"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn non_intrinsic_use_rejects_intrinsic_module() {
        let err =
            resolve_error("program p\n  use, non_intrinsic :: iso_fortran_env\nend program\n");
        assert!(
            err.msg
                .contains("non-intrinsic module 'iso_fortran_env' not found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn non_intrinsic_use_selects_same_named_source_module() {
        let st = resolve_source(
            "module iso_fortran_env\n  integer :: shadow_value\nend module\nprogram p\n  use, non_intrinsic :: iso_fortran_env, only: shadow_value\nend program\n",
        );
        let intrinsic_scope = st.find_intrinsic_module_scope("iso_fortran_env").unwrap();
        let source_scope = st
            .find_non_intrinsic_module_scope("iso_fortran_env")
            .unwrap();
        assert_ne!(intrinsic_scope, source_scope);
        assert!(!st
            .scope(intrinsic_scope)
            .symbols
            .contains_key("shadow_value"));
        assert!(st.scope(source_scope).symbols.contains_key("shadow_value"));

        let program_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(scope.kind, ScopeKind::Program(_)))
            .unwrap();
        assert!(program_scope
            .use_associations
            .iter()
            .any(|association| association.source_scope == source_scope));
    }

    #[test]
    fn normal_use_prefers_same_named_source_module() {
        let st = resolve_source(
            "module iso_fortran_env\n  integer :: shadow_value\nend module\nprogram p\n  use iso_fortran_env, only: shadow_value\nend program\n",
        );
        let source_scope = st
            .find_non_intrinsic_module_scope("iso_fortran_env")
            .unwrap();
        let program_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(scope.kind, ScopeKind::Program(_)))
            .unwrap();
        assert!(program_scope
            .use_associations
            .iter()
            .any(|association| association.source_scope == source_scope));
    }

    #[test]
    fn duplicate_module_units_are_rejected_case_insensitively() {
        let err = resolve_error("module shared_name\nend module\nmodule ShArEd_NaMe\nend module\n");
        assert!(
            err.msg
                .contains("duplicate module program unit 'ShArEd_NaMe'"),
            "unexpected error: {err}"
        );
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
    fn ordinary_type_declarations_reject_existing_local_symbols() {
        let cases = [
            (
                "duplicate in one declaration",
                "\
program duplicate_in_one_declaration
  implicit none
  integer :: value, VALUE
end program duplicate_in_one_declaration
",
                "VALUE",
            ),
            (
                "duplicate local declarations",
                "\
program duplicate_local_declarations
  implicit none
  integer :: value
  real :: VALUE
end program duplicate_local_declarations
",
                "VALUE",
            ),
            (
                "redeclared dummy argument",
                "\
subroutine redeclared_dummy(value)
  implicit none
  integer, intent(in) :: value
  real, intent(in) :: VALUE
end subroutine redeclared_dummy
",
                "VALUE",
            ),
            (
                "retyped explicit function result",
                "\
integer function retyped_result()
  implicit none
  real :: RETYPED_RESULT
  retyped_result = 1.0
end function retyped_result
",
                "RETYPED_RESULT",
            ),
            (
                "dummy argument replaced by parameter",
                "\
subroutine parameter_dummy(value)
  implicit none
  integer, parameter :: VALUE = 1
end subroutine parameter_dummy
",
                "VALUE",
            ),
            (
                "function result replaced by parameter",
                "\
function parameter_result() result(value)
  implicit none
  integer, parameter :: VALUE = 1
end function parameter_result
",
                "VALUE",
            ),
            (
                "parameter replaced by variable",
                "\
module replaced_parameter_m
  implicit none
  integer, parameter :: value = 1
  real :: VALUE
end module replaced_parameter_m
",
                "VALUE",
            ),
            (
                "derived type name replaced by variable",
                "\
module replaced_type_name_m
  implicit none
  type :: payload
    integer :: value
  end type payload
  integer :: PAYLOAD
end module replaced_type_name_m
",
                "PAYLOAD",
            ),
        ];

        for (label, source, repeated_name) in cases {
            let err = resolve_error(source);
            assert_eq!(
                err.msg,
                format!("symbol '{repeated_name}' already defined in this scope"),
                "unexpected declaration-collision diagnostic for {label}"
            );
        }
    }

    #[test]
    fn ordinary_type_declarations_complete_dummy_and_result_placeholders_once() {
        let st = resolve_source(
            "\
module declaration_placeholders_m
  implicit none
  abstract interface
    integer function callback_interface()
    end function callback_interface
  end interface
contains
  subroutine accept_value(value)
    integer, intent(in), optional :: value
  end subroutine accept_value

  subroutine accept_callback(callback)
    procedure(callback_interface), pointer, intent(in) :: callback
  end subroutine accept_callback

  function make_value() result(value)
    integer :: value
    value = 7
  end function make_value

  function make_callback() result(callback)
    procedure(callback_interface), pointer :: callback
  end function make_callback
end module declaration_placeholders_m
",
        );

        let dummy_scope = st
            .scopes
            .iter()
            .find(
                |scope| matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "accept_value"),
            )
            .unwrap();
        let dummy = dummy_scope.symbols.get("value").unwrap();
        assert_eq!(dummy.kind, SymbolKind::Variable);
        assert_eq!(dummy.type_info, Some(TypeInfo::Integer { kind: None }));
        assert_eq!(dummy.attrs.intent, Some(Intent::In));
        assert!(dummy.attrs.optional);

        let procedure_dummy_scope = st
            .scopes
            .iter()
            .find(|scope| {
                matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "accept_callback")
            })
            .unwrap();
        let procedure_dummy = procedure_dummy_scope.symbols.get("callback").unwrap();
        assert_eq!(procedure_dummy.kind, SymbolKind::ProcedurePointer);

        let function_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Function(name) if name == "make_value"))
            .unwrap();
        let result = function_scope.procedure_result_symbol().unwrap();
        assert_eq!(result.kind, SymbolKind::Variable);
        assert_eq!(result.type_info, Some(TypeInfo::Integer { kind: None }));

        let procedure_result_scope = st
            .scopes
            .iter()
            .find(
                |scope| matches!(&scope.kind, ScopeKind::Function(name) if name == "make_callback"),
            )
            .unwrap();
        let procedure_result = procedure_result_scope.procedure_result_symbol().unwrap();
        assert_eq!(procedure_result.kind, SymbolKind::ProcedurePointer);
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
    fn contains_rejects_conflicting_local_identifiers() {
        for source in [
            "\
program collision
  implicit none
  integer :: child
contains
  subroutine child()
  end subroutine child
end program collision
",
            "\
program collision
  implicit none
  integer, parameter :: child = 1
contains
  integer function child()
    child = 2
  end function child
end program collision
",
            "\
subroutine outer(child)
  implicit none
  integer, intent(in) :: child
contains
  subroutine child()
  end subroutine child
end subroutine outer
",
            "\
program collision
  implicit none
  type :: child
  end type child
contains
  integer function child()
    child = 1
  end function child
end program collision
",
            "\
program collision
  implicit none
contains
  subroutine child()
  end subroutine child
  subroutine child()
  end subroutine child
end program collision
",
        ] {
            let err = resolve_error(source);
            assert_eq!(
                err.msg, "symbol 'child' already defined in this scope",
                "unexpected collision diagnostic for source:\n{source}"
            );
        }
    }

    #[test]
    fn contains_allows_a_generic_name_to_name_its_own_specific() {
        let st = resolve_source(
            "\
module same_name_host
  implicit none
  interface child
    module procedure child
    module procedure child_real
  end interface child
contains
  integer function child(value)
    integer, intent(in) :: value
    child = value + 1
  end function child
  integer function child_real(value)
    real, intent(in) :: value
    child_real = int(value) + 2
  end function child_real
end module same_name_host
",
        );
        let module_scope = st
            .scopes
            .iter()
            .find(
                |scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "same_name_host"),
            )
            .unwrap();
        let procedure = module_scope.symbols.get("child").unwrap();
        assert_eq!(procedure.kind, SymbolKind::Function);
        let generic = st
            .named_interface_facet_symbol_in_scope(module_scope.id, "child")
            .unwrap();
        assert_eq!(generic.arg_names, ["child", "child_real"]);
        assert!(st.scopes.iter().any(|scope| {
            scope.parent == Some(module_scope.id)
                && matches!(&scope.kind, ScopeKind::Function(name) if name == "child")
        }));
    }

    #[test]
    fn named_generic_rejects_repeated_specific_declarations() {
        let cases = [
            (
                "same statement",
                "\
module duplicate_same_statement_m
  implicit none
  interface generic_value
    module procedure integer_value, INTEGER_VALUE
  end interface generic_value
contains
  integer function integer_value(value)
    integer, intent(in) :: value
    integer_value = value
  end function integer_value
end module duplicate_same_statement_m
",
                "generic_value",
                "integer_value",
            ),
            (
                "separate statements",
                "\
module duplicate_across_statements_m
  implicit none
  interface generic_value
    module procedure integer_value
    procedure :: INTEGER_VALUE
  end interface generic_value
contains
  integer function integer_value(value)
    integer, intent(in) :: value
    integer_value = value
  end function integer_value
end module duplicate_across_statements_m
",
                "generic_value",
                "integer_value",
            ),
            (
                "reopened interface",
                "\
module duplicate_reopened_interface_m
  implicit none
  interface generic_value
    module procedure integer_value
  end interface generic_value
  interface generic_value
    module procedure INTEGER_VALUE
  end interface generic_value
contains
  integer function integer_value(value)
    integer, intent(in) :: value
    integer_value = value
  end function integer_value
end module duplicate_reopened_interface_m
",
                "generic_value",
                "integer_value",
            ),
            (
                "imported specific",
                "\
module duplicate_import_base_m
  implicit none
  interface generic_value
    module procedure integer_value
  end interface generic_value
contains
  integer function integer_value(value)
    integer, intent(in) :: value
    integer_value = value
  end function integer_value
end module duplicate_import_base_m

module duplicate_import_extension_m
  use duplicate_import_base_m, only: generic_value, integer_value
  implicit none
  interface generic_value
    module procedure INTEGER_VALUE
  end interface generic_value
end module duplicate_import_extension_m
",
                "generic_value",
                "integer_value",
            ),
            (
                "defined operator",
                "\
module duplicate_operator_specific_m
  implicit none
  interface operator(+)
    module procedure add_integer, ADD_INTEGER
  end interface operator(+)
contains
  integer function add_integer(left, right)
    integer, intent(in) :: left, right
    add_integer = left + right
  end function add_integer
end module duplicate_operator_specific_m
",
                "operator(+)",
                "add_integer",
            ),
        ];

        for (label, source, generic_name, specific_name) in cases {
            let err = resolve_error(source);
            assert_eq!(
                err.msg,
                format!(
                    "specific procedure '{specific_name}' is already present in generic interface \
                     '{generic_name}'"
                ),
                "unexpected duplicate-specific diagnostic for {label}"
            );
        }
    }

    #[test]
    fn named_generic_rejects_mixed_function_and_subroutine_specifics() {
        let cases = [
            (
                "module procedures",
                "\
module mixed_module_procedures_m
  implicit none
  interface dispatch
    module procedure compute_value, update_value
  end interface dispatch
contains
  integer function compute_value(value)
    integer, intent(in) :: value
    compute_value = value
  end function compute_value
  subroutine update_value(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine update_value
end module mixed_module_procedures_m
",
            ),
            (
                "bare procedure statements",
                "\
module mixed_procedure_statements_m
  implicit none
  interface dispatch
    procedure :: compute_value
    procedure :: update_value
  end interface dispatch
contains
  integer function compute_value(value)
    integer, intent(in) :: value
    compute_value = value
  end function compute_value
  subroutine update_value(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine update_value
end module mixed_procedure_statements_m
",
            ),
            (
                "explicit interface bodies",
                "\
module mixed_explicit_bodies_m
  implicit none
  interface dispatch
    integer function compute_value(value)
      integer, intent(in) :: value
    end function compute_value
    subroutine update_value(value)
      integer, intent(inout) :: value
    end subroutine update_value
  end interface dispatch
end module mixed_explicit_bodies_m
",
            ),
            (
                "reopened interface",
                "\
module mixed_reopened_interface_m
  implicit none
  interface dispatch
    module procedure compute_value
  end interface dispatch
  interface DISPATCH
    module procedure update_value
  end interface DISPATCH
contains
  integer function compute_value(value)
    integer, intent(in) :: value
    compute_value = value
  end function compute_value
  subroutine update_value(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine update_value
end module mixed_reopened_interface_m
",
            ),
            (
                "use-associated extension",
                "\
module function_generic_provider_m
  implicit none
  interface dispatch
    module procedure compute_value
  end interface dispatch
contains
  integer function compute_value(value)
    integer, intent(in) :: value
    compute_value = value
  end function compute_value
end module function_generic_provider_m

module mixed_generic_extension_m
  use function_generic_provider_m, only: dispatch
  implicit none
  interface dispatch
    module procedure update_value
  end interface dispatch
contains
  subroutine update_value(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine update_value
end module mixed_generic_extension_m
",
            ),
            (
                "merged use-associated generics",
                "\
module function_generic_m
  implicit none
  interface dispatch
    module procedure compute_value
  end interface dispatch
contains
  integer function compute_value(value)
    integer, intent(in) :: value
    compute_value = value
  end function compute_value
end module function_generic_m

module subroutine_generic_m
  implicit none
  interface dispatch
    module procedure update_value
  end interface dispatch
contains
  subroutine update_value(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine update_value
end module subroutine_generic_m

program mixed_use_association
  use function_generic_m
  use subroutine_generic_m
  implicit none
end program mixed_use_association
",
            ),
        ];

        for (label, source) in cases {
            let err = resolve_error(source);
            assert_eq!(
                err.msg,
                "generic interface 'dispatch' may not mix function and subroutine specific procedures",
                "unexpected mixed-procedure diagnostic for {label}"
            );
        }
    }

    #[test]
    fn named_generic_allows_distinct_extensions_and_duplicate_use_paths() {
        let st = resolve_source(
            "\
module generic_base_m
  implicit none
  interface generic_value
    module procedure integer_value
  end interface generic_value
contains
  integer function integer_value(value)
    integer, intent(in) :: value
    integer_value = value
  end function integer_value
end module generic_base_m

module generic_left_m
  use generic_base_m, only: generic_value
end module generic_left_m

module generic_right_m
  use generic_base_m, only: generic_value
end module generic_right_m

module generic_extension_m
  use generic_left_m, only: generic_value
  use generic_right_m, only: generic_value
  implicit none
  interface generic_value
    module procedure real_value
  end interface generic_value
  interface generic_value
    procedure :: logical_value
  end interface generic_value
contains
  integer function real_value(value)
    real, intent(in) :: value
    real_value = int(value)
  end function real_value
  integer function logical_value(value)
    logical, intent(in) :: value
    logical_value = merge(1, 0, value)
  end function logical_value
end module generic_extension_m
",
        );
        let extension_scope = st
            .scopes
            .iter()
            .find(
                |scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "generic_extension_m"),
            )
            .unwrap();
        let generic = st
            .named_interface_symbol_in_scope(extension_scope.id, "generic_value")
            .unwrap();
        assert_eq!(
            generic.arg_names,
            ["integer_value", "real_value", "logical_value"]
        );
    }

    #[test]
    fn named_generic_allows_same_specific_spelling_from_a_different_owner() {
        resolve_source(
            "\
module same_name_owner_base_m
  implicit none
  private
  public :: select_value
  interface select_value
    module procedure pick
  end interface select_value
contains
  integer function pick(value)
    integer, intent(in) :: value
    pick = value
  end function pick
end module same_name_owner_base_m

module same_name_owner_extension_m
  use same_name_owner_base_m, only: select_value
  implicit none
  interface select_value
    module procedure pick
  end interface select_value
contains
  integer function pick(value)
    real, intent(in) :: value
    pick = int(value)
  end function pick
end module same_name_owner_extension_m
",
        );
    }

    #[test]
    fn named_generic_ignores_unlisted_local_shadow_of_inherited_specific() {
        resolve_source(
            "\
module shadow_base_m
  implicit none
  private
  public :: dispatch
  interface dispatch
    module procedure pick
  end interface dispatch
contains
  integer function pick(value)
    integer, intent(in) :: value
    pick = value
  end function pick
end module shadow_base_m

module shadow_extension_m
  use shadow_base_m, only: dispatch
  implicit none
  interface dispatch
    module procedure pick_real
  end interface dispatch
contains
  subroutine pick(value)
    integer, intent(inout) :: value
    value = value + 1
  end subroutine pick
  integer function pick_real(value)
    real, intent(in) :: value
    pick_real = int(value)
  end function pick_real
end module shadow_extension_m
",
        );
    }

    #[test]
    fn local_generic_shadows_instead_of_extending_host_generic() {
        let st = resolve_source(
            "\
module host_generic_m
  implicit none
  interface dispatch
    module procedure host_value
  end interface dispatch
contains
  integer function host_value(value)
    integer, intent(in) :: value
    host_value = value
  end function host_value

  subroutine nested_scope()
    interface dispatch
      procedure :: host_value
    end interface dispatch
  contains
    subroutine host_value(value)
      integer, intent(inout) :: value
      value = value + 1
    end subroutine host_value
  end subroutine nested_scope
end module host_generic_m
",
        );
        let nested_scope = st
            .scopes
            .iter()
            .find(
                |scope| matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "nested_scope"),
            )
            .unwrap();
        let generic = st
            .named_interface_symbol_in_scope(nested_scope.id, "dispatch")
            .unwrap();
        assert_eq!(generic.arg_names, ["host_value"]);
    }

    #[test]
    fn repeated_accessibility_specifications_are_rejected() {
        let cases = [
            (
                "same access in separate statements",
                "\
module repeated_same_access_m
  implicit none
  integer :: value
  public :: value
  public :: VALUE
end module repeated_same_access_m
",
                "accessibility of 'value' is specified more than once in this scope",
            ),
            (
                "conflicting access in separate statements",
                "\
module repeated_conflicting_access_m
  implicit none
  integer :: value
  public :: value
  private :: value
end module repeated_conflicting_access_m
",
                "accessibility of 'value' is specified more than once in this scope",
            ),
            (
                "duplicate name in one statement",
                "\
module repeated_in_one_access_list_m
  implicit none
  integer :: value
  public :: value, VALUE
end module repeated_in_one_access_list_m
",
                "accessibility of 'value' is specified more than once in this scope",
            ),
            (
                "declaration attribute before access statement",
                "\
module attribute_then_access_list_m
  implicit none
  integer, public :: value
  private :: value
end module attribute_then_access_list_m
",
                "accessibility of 'value' is specified more than once in this scope",
            ),
            (
                "access statement before declaration attribute",
                "\
module access_list_then_attribute_m
  implicit none
  public :: value
  integer, private :: value
end module access_list_then_attribute_m
",
                "accessibility of 'value' is specified more than once in this scope",
            ),
            (
                "duplicate declaration attributes",
                "\
module repeated_access_attribute_m
  implicit none
  integer, public, PUBLIC :: value
end module repeated_access_attribute_m
",
                "accessibility of 'value' is specified more than once in this scope",
            ),
            (
                "conflicting declaration attributes",
                "\
module conflicting_access_attributes_m
  implicit none
  integer, public, private :: value
end module conflicting_access_attributes_m
",
                "accessibility of 'value' is specified more than once in this scope",
            ),
            (
                "duplicate derived type attributes",
                "\
module repeated_type_access_attribute_m
  implicit none
  type, public, PUBLIC :: item
    integer :: value
  end type item
end module repeated_type_access_attribute_m
",
                "accessibility of 'item' is specified more than once in this scope",
            ),
            (
                "derived type attribute and access statement",
                "\
module repeated_type_access_m
  implicit none
  type, public :: item
    integer :: value
  end type item
  public :: item
end module repeated_type_access_m
",
                "accessibility of 'item' is specified more than once in this scope",
            ),
            (
                "same default access",
                "\
module repeated_default_access_m
  implicit none
  private
  private
end module repeated_default_access_m
",
                "default accessibility is specified more than once in this scope",
            ),
            (
                "conflicting default access",
                "\
module conflicting_default_access_m
  implicit none
  private
  public
end module conflicting_default_access_m
",
                "default accessibility is specified more than once in this scope",
            ),
            (
                "generic access before generic declaration",
                "\
module repeated_generic_access_m
  implicit none
  public :: dispatch
  private :: DISPATCH
  interface dispatch
    module procedure compute_value
  end interface dispatch
contains
  integer function compute_value(value)
    integer, intent(in) :: value
    compute_value = value
  end function compute_value
end module repeated_generic_access_m
",
                "accessibility of 'dispatch' is specified more than once in this scope",
            ),
        ];

        for (label, source, expected) in cases {
            let err = resolve_error(source);
            assert_eq!(
                err.msg, expected,
                "unexpected repeated-access diagnostic for {label}"
            );
        }
    }

    #[test]
    fn default_access_and_one_named_override_are_not_repetitions() {
        resolve_source(
            "\
module legal_access_override_m
  implicit none
  private
  public :: visible
  integer :: hidden
  integer :: visible
end module legal_access_override_m
",
        );
    }

    #[test]
    fn large_accessibility_ledger_accepts_unique_names_and_finds_a_late_repeat() {
        let names = (0..4096)
            .map(|index| format!("value_{index:04}"))
            .collect::<Vec<_>>()
            .join(", ");
        let valid = format!(
            "\
module large_unique_access_m
  implicit none
  integer, public :: {names}
end module large_unique_access_m
"
        );
        resolve_source(&valid);

        let invalid = format!(
            "\
module large_repeated_access_m
  implicit none
  integer, public :: {names}
  private :: VALUE_0000
end module large_repeated_access_m
"
        );
        let err = resolve_error(&invalid);
        assert_eq!(
            err.msg,
            "accessibility of 'value_0000' is specified more than once in this scope"
        );
    }

    #[test]
    fn interface_body_registration_rejects_conflicting_local_identifiers() {
        for source in [
            "\
module collision
  implicit none
  integer :: collide
  interface
    subroutine collide()
    end subroutine collide
  end interface
end module collision
",
            "\
module collision
  implicit none
  interface
    subroutine collide()
    end subroutine collide
  end interface
  integer :: COLLIDE
end module collision
",
            "\
module collision
  implicit none
  integer, parameter :: collide = 1
  interface
    integer function collide()
    end function collide
  end interface
end module collision
",
            "\
module collision
  implicit none
  type :: collide
    integer :: value
  end type collide
  interface
    subroutine collide()
    end subroutine collide
  end interface
end module collision
",
            "\
module collision
  implicit none
  interface
    integer function collide()
    end function collide
  end interface
  interface
    subroutine collide()
    end subroutine collide
  end interface
end module collision
",
            "\
module collision
  implicit none
  interface
    subroutine collide()
    end subroutine collide
  end interface
contains
  subroutine collide()
  end subroutine collide
end module collision
",
            "\
subroutine outer(collide)
  implicit none
  integer, intent(in) :: collide(:)
  interface
    subroutine collide(value)
      integer, intent(in) :: value
    end subroutine collide
  end interface
end subroutine outer
",
            "\
subroutine outer(collide)
  implicit none
  integer, value :: collide
  interface
    subroutine collide()
    end subroutine collide
  end interface
end subroutine outer
",
            "\
subroutine outer(collide)
  implicit none
  integer, allocatable :: collide
  interface
    subroutine collide()
    end subroutine collide
  end interface
end subroutine outer
",
            "\
subroutine outer(collide)
  implicit none
  integer, target :: collide
  interface
    subroutine collide()
    end subroutine collide
  end interface
end subroutine outer
",
            "\
subroutine outer(collide)
  implicit none
  integer, save :: collide
  interface
    subroutine collide()
    end subroutine collide
  end interface
end subroutine outer
",
            "\
subroutine outer(collide)
  implicit none
  integer, intrinsic :: collide
  interface
    subroutine collide()
    end subroutine collide
  end interface
end subroutine outer
",
            "\
subroutine outer(collide)
  implicit none
  integer, external :: collide
  interface
    integer function collide()
    end function collide
  end interface
end subroutine outer
",
        ] {
            let err = resolve_error(source);
            assert_eq!(
                err.msg, "symbol 'collide' already defined in this scope",
                "unexpected interface collision diagnostic for source:\n{source}"
            );
        }
    }

    #[test]
    fn interface_body_registration_merges_a_procedure_dummy() {
        let st = resolve_source(
            "\
subroutine invoke(callback)
  implicit none
  external :: callback
  interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
end subroutine invoke
",
        );
        let invoke = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "invoke"))
            .expect("missing invoke scope");
        let callback = invoke
            .symbols
            .get("callback")
            .expect("procedure dummy interface was not published");
        assert_eq!(callback.kind, SymbolKind::Subroutine);
        assert!(callback.attrs.external);
        assert_eq!(callback.arg_names, ["value"]);
    }

    #[test]
    fn interface_body_registration_preserves_compatible_dummy_attributes() {
        let st = resolve_source(
            "\
subroutine invoke(callback)
  implicit none
  integer, pointer, optional :: callback
  interface
    subroutine callback(value)
      integer, intent(in) :: value
    end subroutine callback
  end interface
end subroutine invoke
",
        );
        let invoke = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "invoke"))
            .expect("missing invoke scope");
        let callback = invoke
            .symbols
            .get("callback")
            .expect("procedure dummy interface was not published");
        assert_eq!(callback.kind, SymbolKind::Subroutine);
        assert!(callback.attrs.external);
        assert!(callback.attrs.pointer);
        assert!(callback.attrs.optional);
        assert_eq!(callback.arg_names, ["value"]);
    }

    #[test]
    fn bind_c_name_resolves_named_character_constant() {
        let st = resolve_source(
            "\
module bindings
  implicit none
  character(len=*), parameter :: exported_name = 'armfortas_named_binding'
contains
  subroutine set_answer(value) bind(c, name=exported_name)
    integer, intent(out) :: value
    value = 42
  end subroutine
end module
",
        );
        let module_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "bindings"))
            .unwrap();
        assert_eq!(
            module_scope.symbols["set_answer"]
                .attrs
                .binding_label
                .as_deref(),
            Some("armfortas_named_binding")
        );
        assert!(module_scope.symbols["set_answer"].attrs.bind_c);
    }

    #[test]
    fn bind_c_name_resolves_concatenated_character_constants() {
        let st = resolve_source(
            "\
module bindings
  implicit none
  character(len=*), parameter :: prefix = 'armfortas_'
  character(len=*), parameter :: suffix = 'concat_binding'
contains
  subroutine set_answer(value) bind(c, name=prefix // suffix)
    integer, intent(out) :: value
  end subroutine
end module
",
        );
        let module_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "bindings"))
            .unwrap();
        assert_eq!(
            module_scope.symbols["set_answer"]
                .attrs
                .binding_label
                .as_deref(),
            Some("armfortas_concat_binding")
        );
    }

    #[test]
    fn bind_c_name_resolves_local_character_constant() {
        let st = resolve_source(
            "\
subroutine set_answer(value) bind(c, name=exported_name)
  character(len=*), parameter :: exported_name = 'armfortas_local_binding'
  integer, intent(out) :: value
end subroutine
",
        );
        let proc_scope = st
            .scopes
            .iter()
            .find(
                |scope| matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "set_answer"),
            )
            .unwrap();
        assert_eq!(
            proc_scope.binding_label.as_deref(),
            Some("armfortas_local_binding")
        );
        assert!(proc_scope.bind_c);
    }

    #[test]
    fn bind_c_name_resolves_imported_interface_constant() {
        let st = resolve_source(
            "\
module bindings
  implicit none
  character(len=*), parameter :: exported_name = 'armfortas_iface_binding'
  interface
    subroutine set_answer(value) bind(c, name=exported_name)
      import :: exported_name
      integer, intent(out) :: value
    end subroutine
  end interface
end module
",
        );
        let module_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "bindings"))
            .unwrap();
        assert_eq!(
            module_scope.symbols["set_answer"]
                .attrs
                .binding_label
                .as_deref(),
            Some("armfortas_iface_binding")
        );
    }

    #[test]
    fn separate_module_body_inherits_resolved_bind_c_name() {
        let st = resolve_source(
            "\
module bindings
  implicit none
  character(len=*), parameter :: exported_name = 'armfortas_smp_binding'
  interface
    module subroutine set_answer(value) bind(c, name=exported_name)
      import :: exported_name
      integer, intent(out) :: value
    end subroutine
  end interface
end module
submodule(bindings) implementation
contains
  module procedure set_answer
    value = 42
  end procedure set_answer
end submodule
",
        );
        let submodule_scope = st
            .scopes
            .iter()
            .find(|scope| {
                matches!(&scope.kind, ScopeKind::Submodule(name) if name == "implementation")
            })
            .unwrap();
        let body_scope = st
            .scopes
            .iter()
            .find(|scope| {
                scope.parent == Some(submodule_scope.id)
                    && matches!(
                        &scope.kind,
                        ScopeKind::Subroutine(name) if name == "set_answer"
                    )
            })
            .unwrap();
        assert_eq!(
            body_scope.binding_label.as_deref(),
            Some("armfortas_smp_binding")
        );
        assert!(body_scope.bind_c);
        assert_eq!(
            submodule_scope.symbols["set_answer"]
                .attrs
                .binding_label
                .as_deref(),
            Some("armfortas_smp_binding")
        );
        assert!(submodule_scope.symbols["set_answer"].attrs.bind_c);
    }

    #[test]
    fn bind_c_name_rejects_nonconstant_character_entity() {
        let err = resolve_error(
            "\
module bindings
  implicit none
  character(len=32) :: exported_name = 'armfortas_named_binding'
contains
  subroutine set_answer(value) bind(c, name=exported_name)
    integer, intent(out) :: value
  end subroutine
end module
",
        );
        assert!(
            err.msg
                .contains("BIND(C) NAME= must be a scalar default-character constant expression"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn bind_c_name_rejects_noncharacter_and_unknown_expressions() {
        for source in [
            "subroutine bad() bind(c, name=42)\nend subroutine\n",
            "subroutine bad() bind(c, name=missing_name)\nend subroutine\n",
            "subroutine bad() bind(c, name=wide_name)\n  character(kind=4, len=*), parameter :: wide_name = 'wide'\nend subroutine\n",
            "subroutine bad() bind(c, name=names)\n  character(len=3), parameter :: names(2) = 'foo'\nend subroutine\n",
        ] {
            let err = resolve_error(source);
            assert!(
                err.msg.contains(
                    "BIND(C) NAME= must be a scalar default-character constant expression"
                ),
                "unexpected error for {source:?}: {err}"
            );
        }
    }

    #[test]
    fn bind_c_name_rejects_invalid_c_identifier() {
        let err = resolve_error("subroutine bad() bind(c, name='not-a-c-name')\nend subroutine\n");
        assert!(
            err.msg
                .contains("must evaluate to a valid C identifier or an empty string"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn empty_bind_c_name_has_no_binding_label() {
        let st = resolve_source(
            "\
module bindings
contains
  subroutine native_name() bind(c, name='')
  end subroutine
end module
",
        );
        let proc_scope = st
            .scopes
            .iter()
            .find(
                |scope| matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "native_name"),
            )
            .unwrap();
        assert!(proc_scope.bind_c);
        assert_eq!(proc_scope.binding_label, None);
        let module_scope = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "bindings"))
            .unwrap();
        assert!(module_scope.symbols["native_name"].attrs.bind_c);
        assert_eq!(
            module_scope.symbols["native_name"].attrs.binding_label,
            None
        );
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

    #[test]
    fn character_parameter_length_inference_is_lexically_scoped() {
        for source in [
            "\
module unrelated
  implicit none
  character(8), parameter :: seed = '12345678'
end module unrelated

module victim
  implicit none
  character(1), parameter :: seed = 'Z'
  character(*), parameter :: direct = seed
  character(*), parameter :: parenthesized = (seed)
  character(*), parameter :: concatenated = seed // seed
  character(*), parameter :: repeated = repeat(seed, 2)
end module victim
",
            "\
module victim
  implicit none
  character(1), parameter :: seed = 'Z'
  character(*), parameter :: direct = seed
  character(*), parameter :: parenthesized = (seed)
  character(*), parameter :: concatenated = seed // seed
  character(*), parameter :: repeated = repeat(seed, 2)
end module victim

module unrelated
  implicit none
  character(8), parameter :: seed = '12345678'
end module unrelated
",
        ] {
            let st = resolve_source(source);
            let victim = st
                .scopes
                .iter()
                .find(|scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "victim"))
                .expect("missing victim module scope");

            for (name, expected_len) in [
                ("direct", 1),
                ("parenthesized", 1),
                ("concatenated", 2),
                ("repeated", 2),
            ] {
                assert!(
                    matches!(
                        victim.symbols[name].type_info,
                        Some(TypeInfo::Character {
                            len: Some(actual),
                            ..
                        }) if actual == expected_len
                    ),
                    "{name} inferred the wrong length in source:\n{source}"
                );
            }
        }
    }

    #[test]
    fn character_parameter_length_inference_honors_use_and_host_association() {
        let st = resolve_source(
            "\
module provider
  implicit none
  character(3), parameter :: imported = 'abc'
end module provider

module consumer
  use provider, only: imported
  implicit none
  character(*), parameter :: use_copy = imported
contains
  subroutine nested
    character(*), parameter :: host_copy = use_copy
  end subroutine nested
end module consumer
",
        );

        let consumer = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Module(name) if name == "consumer"))
            .expect("missing consumer module scope");
        assert!(matches!(
            consumer.symbols["use_copy"].type_info,
            Some(TypeInfo::Character { len: Some(3), .. })
        ));

        let nested = st
            .scopes
            .iter()
            .find(|scope| matches!(&scope.kind, ScopeKind::Subroutine(name) if name == "nested"))
            .expect("missing nested subroutine scope");
        assert!(matches!(
            nested.symbols["host_copy"].type_info,
            Some(TypeInfo::Character { len: Some(3), .. })
        ));
    }
}
