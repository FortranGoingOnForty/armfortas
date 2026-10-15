//! ARMFORTAS module file (.amod) writer and reader.
//!
//! Format spec: see `.claude/plans/composed-questing-catmull.md`.
//!
//! The `.amod` file is a human-readable, self-documenting, diffable
//! description of a Fortran module's public interface — carrying
//! enough information for full ABI-correct separate compilation.
//!
//! Innovations over gfortran/flang/ifort:
//!   - Linker symbol names (@ir) for direct FFI
//!   - Polymorphic type tags (@tag)
//!   - Human-editable for hand-written FFI descriptions

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

use crate::ast::decl::UseNature;
use crate::driver::defaults::DefaultKinds;
use crate::ir::inst::Module as IrModule;
use crate::ir::lower::ModuleGlobalInfo;
use crate::sema::symtab::*;
use crate::sema::type_layout::{
    concretize_intrinsic_type_info, BoundProc, TypeLayout, TypeLayoutRegistry,
};

const AMOD_VERSION: u32 = 19;
const AMOD_TYPE_ACCESS_VERSION: u32 = 9;
const AMOD_FIELD_ACCESS_VERSION: u32 = 10;
const AMOD_FINAL_ELEMENTAL_VERSION: u32 = 12;
const AMOD_DEFAULT_ACCESS_VERSION: u32 = 13;
pub(crate) const SMOD_VERSION: u32 = 2;

/// Stringify a Vec<ArraySpec> as `(dim1; dim2; ...)` where each dim
/// is `lower:upper` or just `upper`. Returns None if any dim is not
/// `Explicit` (assumed-shape, deferred, etc. round-trip via the
/// existing rank-based reconstruction in `load_external_module`).
///
/// Used to preserve runtime-shape result bounds across split-file
/// submodule compilation. Examples:
///   `(n)`             → `Explicit { lower: None, upper: Name(n) }`
///   `(max(n, 0))`     → `Explicit { lower: None, upper: max(n,0) }`
///   `(1:n, 1:m)`      → two-dim Explicit with both bounds
fn stringify_array_bounds(specs: &[crate::ast::decl::ArraySpec]) -> Option<String> {
    use crate::ast::decl::ArraySpec;
    let mut parts: Vec<String> = Vec::with_capacity(specs.len());
    for spec in specs {
        match spec {
            ArraySpec::Explicit { lower, upper } => {
                let upper_s = upper.to_sexpr();
                if let Some(lo) = lower {
                    parts.push(format!("{}:{}", lo.to_sexpr(), upper_s));
                } else {
                    parts.push(upper_s);
                }
            }
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("({})", parts.join("; ")))
}

/// Parse a `(dim1; dim2; ...)`-encoded array bounds string back into
/// a Vec<ArraySpec> by re-lexing and re-parsing each bound expression
/// via the regular Fortran parser. Returns None if the string is
/// malformed or any bound expr fails to parse — in that case the
/// loader falls back to its rank-based AssumedShape reconstruction.
pub(crate) fn parse_array_bounds(s: &str) -> Option<Vec<crate::ast::decl::ArraySpec>> {
    use crate::ast::decl::ArraySpec;
    let inner = s.strip_prefix('(').and_then(|s| s.strip_suffix(')'))?;
    let mut specs = Vec::new();
    for dim in inner.split(';') {
        let dim = dim.trim();
        if dim.is_empty() {
            return None;
        }
        let split_at = top_level_colon(dim);
        let (lower_str, upper_str) = match split_at {
            Some(i) => (Some(&dim[..i]), &dim[i + 1..]),
            None => (None, dim),
        };
        let upper = parse_simple_expr(upper_str.trim())?;
        let lower = match lower_str {
            Some(s) => Some(parse_simple_expr(s.trim())?),
            None => None,
        };
        specs.push(ArraySpec::Explicit { lower, upper });
    }
    Some(specs)
}

fn stringify_dummy_array_spec(specs: &[crate::ast::decl::ArraySpec]) -> Option<String> {
    use crate::ast::decl::ArraySpec;
    if specs.is_empty() {
        return None;
    }
    let parts = specs
        .iter()
        .map(|spec| match spec {
            ArraySpec::Explicit { lower, upper } => lower.as_ref().map_or_else(
                || upper.to_sexpr(),
                |lower| format!("{}:{}", lower.to_sexpr(), upper.to_sexpr()),
            ),
            ArraySpec::AssumedShape { lower } => lower
                .as_ref()
                .map_or_else(|| ":".to_string(), |lower| format!("{}:", lower.to_sexpr())),
            ArraySpec::AssumedSize { lower } => lower.as_ref().map_or_else(
                || "*".to_string(),
                |lower| format!("{}:*", lower.to_sexpr()),
            ),
            ArraySpec::Deferred => "deferred".to_string(),
            ArraySpec::AssumedRank => "..".to_string(),
        })
        .collect::<Vec<_>>();
    Some(format!("({})", parts.join("; ")))
}

fn parse_dummy_array_spec(s: &str) -> Option<Vec<crate::ast::decl::ArraySpec>> {
    use crate::ast::decl::ArraySpec;
    let inner = s.strip_prefix('(').and_then(|s| s.strip_suffix(')'))?;
    let mut specs = Vec::new();
    for dim in inner.split(';') {
        let dim = dim.trim();
        let spec = match dim {
            "" => return None,
            ":" => ArraySpec::AssumedShape { lower: None },
            "*" => ArraySpec::AssumedSize { lower: None },
            "deferred" => ArraySpec::Deferred,
            ".." => ArraySpec::AssumedRank,
            _ if dim.ends_with(":*") => ArraySpec::AssumedSize {
                lower: Some(parse_simple_expr(dim[..dim.len() - 2].trim())?),
            },
            _ if dim.ends_with(':') => ArraySpec::AssumedShape {
                lower: Some(parse_simple_expr(dim[..dim.len() - 1].trim())?),
            },
            _ => {
                let (lower, upper) = match top_level_colon(dim) {
                    Some(index) => (
                        Some(parse_simple_expr(dim[..index].trim())?),
                        parse_simple_expr(dim[index + 1..].trim())?,
                    ),
                    None => (None, parse_simple_expr(dim)?),
                };
                ArraySpec::Explicit { lower, upper }
            }
        };
        specs.push(spec);
    }
    Some(specs)
}

fn top_level_colon(text: &str) -> Option<usize> {
    let mut depth: i32 = 0;
    for (index, character) in text.char_indices() {
        match character {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ':' if depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_simple_expr(src: &str) -> Option<crate::ast::expr::SpannedExpr> {
    let tokens = crate::lexer::Lexer::tokenize(src, 0).ok()?;
    let mut parser = crate::parser::Parser::new(&tokens);
    parser.parse_expr().ok()
}

fn hex_encode_bytes(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{:02x}", byte));
    }
    hex
}

fn hex_decode_bytes(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut idx = 0usize;
    while idx < value.len() {
        let next = idx + 2;
        let byte = u8::from_str_radix(&value[idx..next], 16).ok()?;
        bytes.push(byte);
        idx = next;
    }
    Some(bytes)
}

fn encode_nested_field_default_init(init: &crate::sema::type_layout::FieldDefaultInit) -> String {
    use crate::sema::type_layout::FieldDefaultInit;
    match init {
        FieldDefaultInit::Character(value) => format!(
            "C{}",
            hex_encode_bytes(&crate::source_bytes::from_source_view(value))
        ),
        FieldDefaultInit::Integer(value) => format!("I{}", value),
        FieldDefaultInit::Logical(value) => format!("L{}", if *value { '1' } else { '0' }),
        FieldDefaultInit::Real(value) => format!("R{:016x}", value.to_bits()),
        FieldDefaultInit::Derived(fields) => {
            let rendered = fields
                .iter()
                .map(|(name, value)| format!("{name}={}", encode_nested_field_default_init(value)))
                .collect::<Vec<_>>()
                .join(",");
            format!("D({rendered})")
        }
        FieldDefaultInit::ProcedurePointer(target) => {
            format!("P{}", hex_encode_bytes(target.as_bytes()))
        }
    }
}

fn split_nested_default_fields(payload: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (idx, ch) in payload.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&payload[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    if depth != 0 {
        return None;
    }
    if !payload.is_empty() {
        out.push(&payload[start..]);
    }
    Some(out)
}

fn decode_nested_field_default_init(
    encoded: &str,
) -> Option<crate::sema::type_layout::FieldDefaultInit> {
    use crate::sema::type_layout::FieldDefaultInit;
    if let Some(value) = encoded.strip_prefix('C') {
        let decoded = crate::source_bytes::to_source_view(&hex_decode_bytes(value)?);
        return Some(FieldDefaultInit::Character(decoded));
    }
    if let Some(value) = encoded.strip_prefix('I') {
        return value.parse::<i128>().ok().map(FieldDefaultInit::Integer);
    }
    if let Some(value) = encoded.strip_prefix('L') {
        return match value {
            "1" => Some(FieldDefaultInit::Logical(true)),
            "0" => Some(FieldDefaultInit::Logical(false)),
            _ => None,
        };
    }
    if let Some(value) = encoded.strip_prefix('R') {
        return u64::from_str_radix(value, 16)
            .ok()
            .map(f64::from_bits)
            .map(FieldDefaultInit::Real);
    }
    if let Some(value) = encoded.strip_prefix("D(").and_then(|s| s.strip_suffix(')')) {
        let mut fields = Vec::new();
        for entry in split_nested_default_fields(value)? {
            let (name, payload) = entry.split_once('=')?;
            let init = decode_nested_field_default_init(payload)?;
            fields.push((name.to_string(), init));
        }
        return Some(FieldDefaultInit::Derived(fields));
    }
    if let Some(value) = encoded.strip_prefix('P') {
        let decoded = String::from_utf8(hex_decode_bytes(value)?).ok()?;
        return Some(FieldDefaultInit::ProcedurePointer(decoded));
    }
    None
}

// =====================================================================
// Writer
// =====================================================================

fn resolved_module_nature(scope: &Scope) -> UseNature {
    if scope.intrinsic_module {
        UseNature::Intrinsic
    } else {
        UseNature::NonIntrinsic
    }
}

fn module_nature_token(nature: UseNature) -> &'static str {
    match nature {
        UseNature::Normal => "normal",
        UseNature::Intrinsic => "intrinsic",
        UseNature::NonIntrinsic => "non_intrinsic",
    }
}

fn procedure_interface_import(
    st: &SymbolTable,
    procedure_scope: &Scope,
    interface_name: &str,
) -> Option<UseRename> {
    let association = procedure_scope
        .use_associations
        .iter()
        .find(|association| association.local_name.eq_ignore_ascii_case(interface_name))?;
    let source_scope = st.scope(association.source_scope);
    let ScopeKind::Module(source_module) = &source_scope.kind else {
        return None;
    };
    Some(UseRename {
        local: association.local_name.clone(),
        original: association.original_name.clone(),
        source_module: source_module.to_ascii_lowercase(),
        source_nature: resolved_module_nature(source_scope),
    })
}

fn format_module_reference(module_name: &str, nature: UseNature) -> String {
    match nature {
        UseNature::Normal => module_name.to_string(),
        UseNature::Intrinsic => format!("intrinsic :: {module_name}"),
        UseNature::NonIntrinsic => format!("non_intrinsic :: {module_name}"),
    }
}

/// Serialize a module's public interface to the current `.amod` text format.
pub fn write_amod(
    module_name: &str,
    source_path: &str,
    st: &SymbolTable,
    mod_scope_id: ScopeId,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    type_layouts: &TypeLayoutRegistry,
    ir_module: &IrModule,
    descriptor_params: &HashMap<String, Vec<bool>>,
    char_len_star_params: &HashMap<String, Vec<bool>>,
) -> Result<String, String> {
    let mut out = String::new();
    let mod_key = module_name.to_lowercase();
    let scope = st.scope(mod_scope_id);
    let provider_defaults = crate::driver::defaults::current();

    // ---- Header ----
    writeln!(out, "#!amod {}", AMOD_VERSION).unwrap();
    writeln!(out, "# module: {}", mod_key).unwrap();
    writeln!(
        out,
        "# default-access: {}",
        if matches!(scope.default_access, Access::Private) {
            "private"
        } else {
            "public"
        }
    )
    .unwrap();
    if matches!(scope.kind, ScopeKind::Submodule(_)) {
        if let Some(ancestor) = &scope.submodule_ancestor {
            writeln!(out, "# ancestor-module: {}", ancestor.to_lowercase()).unwrap();
        }
        if let Some(parent) = scope.use_associations.iter().find_map(|association| {
            if !association.is_submodule_access || !association.local_name.is_empty() {
                return None;
            }
            match &st.scope(association.source_scope).kind {
                ScopeKind::Submodule(name) => Some(name.to_lowercase()),
                _ => None,
            }
        }) {
            writeln!(out, "# parent-submodule: {}", parent).unwrap();
        }
        match &scope.host_association.policy {
            HostAssociationPolicy::All => {
                writeln!(out, "# host-association: all").unwrap();
            }
            HostAssociationPolicy::None => {
                writeln!(out, "# host-association: none").unwrap();
            }
            HostAssociationPolicy::Only(names) => {
                let mut names: Vec<_> = names.iter().map(String::as_str).collect();
                names.sort_unstable();
                write!(out, "# host-association: only").unwrap();
                for name in names {
                    write!(out, " {}", name).unwrap();
                }
                writeln!(out).unwrap();
            }
        }
    }
    writeln!(out, "# source: {}", source_path).unwrap();
    writeln!(out, "# compiled: {}", compile_timestamp()).unwrap();
    writeln!(out, "# compiler: armfortas 0.1.0").unwrap();
    writeln!(out).unwrap();

    // ---- Dependencies ----
    let mut deps: Vec<AmodModuleDependency> = scope
        .use_associations
        .iter()
        .filter_map(|ua| {
            if !ua.from_bare_use || !ua.local_name.is_empty() {
                return None;
            }
            let src_scope = st.scope(ua.source_scope);
            if let ScopeKind::Module(ref n) = src_scope.kind {
                Some(AmodModuleDependency {
                    module_name: n.to_lowercase(),
                    nature: resolved_module_nature(src_scope),
                })
            } else {
                None
            }
        })
        .collect();
    deps.sort_by(|left, right| left.module_name.cmp(&right.module_name));
    for dep in &deps {
        writeln!(
            out,
            "@uses {}",
            format_module_reference(&dep.module_name, dep.nature)
        )
        .unwrap();
    }
    if !deps.is_empty() {
        writeln!(out).unwrap();
    }

    // Named accessibility statements also govern USE-associated names, which
    // have no local Symbol record on which to carry an access attribute. Keep
    // these overrides explicit so a private-default facade can deliberately
    // re-export selected imports (and a public-default facade can hide them).
    let mut named_access: Vec<_> = scope.pending_access.iter().collect();
    named_access.sort_by_key(|(name, _)| name.as_str());
    for (name, access) in named_access {
        let access = match access {
            Access::Public => "public",
            Access::Private => "private",
            Access::Default => continue,
        };
        writeln!(out, "@access {access} :: {name}").unwrap();
    }
    if !scope.pending_access.is_empty() {
        writeln!(out).unwrap();
    }

    // ONLY-qualified edges must retain their exact local and provider names.
    // Reconstructing them as bare USE edges leaks every public provider symbol
    // through a separately compiled facade.
    let mut only_out: Vec<(String, String, String, UseNature)> = scope
        .use_associations
        .iter()
        .filter_map(|ua| {
            if ua.from_bare_use || ua.local_name.is_empty() {
                return None;
            }
            let src_scope = st.scope(ua.source_scope);
            if let ScopeKind::Module(ref n) = src_scope.kind {
                Some((
                    ua.local_name.clone(),
                    ua.original_name.clone(),
                    n.to_lowercase(),
                    resolved_module_nature(src_scope),
                ))
            } else {
                None
            }
        })
        .collect();
    only_out
        .sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));
    only_out.dedup();
    for (local, original, src, nature) in &only_out {
        writeln!(
            out,
            "@use_only {} = {} from {}",
            local,
            original,
            format_module_reference(src, *nature)
        )
        .unwrap();
    }
    if !only_out.is_empty() {
        writeln!(out).unwrap();
    }

    // ---- Bare USE renames ----
    // Record each `use M, a => b` as `@use_rename a = b from m`.
    // Submodule bodies pulled in by host association need to resolve
    // names like `block_kind` (renamed from `int64`) for kind selectors
    // and intrinsic dispatch; without preserving the rename, the .amod
    // can't reconstruct the kind constant and `integer(block_kind) ::
    // dummy` falls back to the default kind.
    let mut renames_out: Vec<(String, String, String, UseNature)> = scope
        .use_associations
        .iter()
        .filter_map(|ua| {
            if !ua.from_bare_use || ua.local_name == ua.original_name {
                return None;
            }
            let src_scope = st.scope(ua.source_scope);
            if let ScopeKind::Module(ref n) = src_scope.kind {
                Some((
                    ua.local_name.clone(),
                    ua.original_name.clone(),
                    n.to_lowercase(),
                    resolved_module_nature(src_scope),
                ))
            } else {
                None
            }
        })
        .collect();
    renames_out
        .sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));
    for (local, original, src, nature) in &renames_out {
        writeln!(
            out,
            "@use_rename {} = {} from {}",
            local,
            original,
            format_module_reference(src, *nature)
        )
        .unwrap();
    }
    if !renames_out.is_empty() {
        writeln!(out).unwrap();
    }

    // Collect and sort public symbols (used for procedures /
    // interfaces / derived types — those still go out public-only).
    let mut syms: Vec<(&String, &Symbol)> = scope
        .symbols
        .iter()
        .filter(|(_, sym)| is_public(sym, scope))
        .collect();
    syms.sort_by_key(|(k, _)| k.to_lowercase());

    // Per F2008 §11.2.3, submodules see ALL parent entities including
    // private ones.  Variables and parameters are emitted regardless
    // of access; private ones carry a `private` attribute and are
    // filtered out at ordinary USE-association time.
    let mut all_syms: Vec<(&String, &Symbol)> = scope.symbols.iter().collect();
    all_syms.sort_by_key(|(k, _)| k.to_lowercase());

    // ---- Variables ----
    let vars: Vec<_> = all_syms
        .iter()
        .filter(|(_, sym)| {
            matches!(
                sym.kind,
                SymbolKind::Variable | SymbolKind::ProcedurePointer
            ) && !sym.attrs.parameter
        })
        .collect();
    for (name, sym) in &vars {
        emit_variable(&mut out, &mod_key, name, sym, globals, provider_defaults);
    }
    if !vars.is_empty() {
        writeln!(out).unwrap();
    }

    // ---- Parameters ----
    let params: Vec<_> = all_syms
        .iter()
        .filter(|(_, sym)| sym.attrs.parameter || matches!(sym.kind, SymbolKind::Parameter))
        .collect();
    for (name, sym) in &params {
        emit_parameter(&mut out, &mod_key, name, sym, globals, provider_defaults);
    }
    if !params.is_empty() {
        writeln!(out).unwrap();
    }

    // ---- F2023 strict enumeration types ----
    // `@enumtype <access> <name> <e1> <e2> ...`; enumerator ordinals
    // are positional (1-based), so the one record re-registers both
    // the type and its typed constants on USE.
    let enum_syms: Vec<_> = all_syms
        .iter()
        .filter(|(_, sym)| {
            matches!(sym.kind, SymbolKind::EnumerationType)
                && matches!(sym.type_info, Some(TypeInfo::Enumeration(_)))
        })
        .collect();
    for (name, sym) in &enum_syms {
        let access = if matches!(sym.attrs.access, Access::Private) {
            "private"
        } else {
            "public"
        };
        writeln!(
            out,
            "@enumtype {} {} {}",
            access,
            name,
            sym.arg_names.join(" ")
        )
        .unwrap();
    }
    if !enum_syms.is_empty() {
        writeln!(out).unwrap();
    }

    // ---- Procedures ----
    let interface_specifics: BTreeSet<String> = all_syms
        .iter()
        .filter(|(_, sym)| {
            matches!(sym.kind, SymbolKind::NamedInterface)
                || (matches!(sym.kind, SymbolKind::DerivedType) && !sym.arg_names.is_empty())
        })
        .flat_map(|(_, sym)| sym.arg_names.iter().cloned())
        .collect();
    // Descendant submodules inherit private ancestor procedures. Retain every
    // procedure with its access marker; ordinary USE association still filters
    // private entries while submodule host association can reconstruct them.
    let mut proc_export_names: BTreeSet<String> = interface_specifics;
    for (name, sym) in &all_syms {
        if matches!(sym.kind, SymbolKind::Function | SymbolKind::Subroutine) {
            proc_export_names.insert(name.to_lowercase());
        }
    }
    for (name, _sym) in syms
        .iter()
        .filter(|(_, sym)| matches!(sym.kind, SymbolKind::DerivedType))
    {
        if let Some(layout) = type_layouts.get(name) {
            for bp in &layout.bound_procs {
                proc_export_names.insert(bp.abi_name.to_lowercase());
                proc_export_names.insert(bound_proc_target_export_key(&mod_key, &bp.target_name));
            }
        }
    }
    for layout in type_layouts.layouts.values() {
        if layout
            .owner_module
            .as_ref()
            .is_some_and(|owner| owner.eq_ignore_ascii_case(&mod_key))
        {
            for field in &layout.fields {
                if field.procedure_pointer {
                    if let TypeInfo::Derived(signature_name) = &field.type_info {
                        proc_export_names.insert(signature_name.to_lowercase());
                    }
                }
            }
        }
    }
    let mut procs: Vec<_> = scope
        .symbols
        .iter()
        .filter(|(name, sym)| {
            matches!(sym.kind, SymbolKind::Function | SymbolKind::Subroutine)
                && proc_export_names.contains(&name.to_lowercase())
        })
        .collect();
    procs.sort_by_key(|(k, _)| k.to_lowercase());
    for (name, sym) in &procs {
        emit_procedure(
            &mut out,
            name,
            sym,
            st,
            mod_scope_id,
            ir_module,
            descriptor_params,
            char_len_star_params,
            provider_defaults,
        );
    }

    // ---- Types ----
    // Include all derived types, even private ones — submodules need access
    // to their parent module's private types per F2008 12.2.3.2.
    let mut type_exports: BTreeSet<String> = BTreeSet::new();
    for (name, sym) in &syms {
        if matches!(sym.kind, SymbolKind::DerivedType) {
            collect_exported_type_closure(&mut type_exports, name, type_layouts);
        }
        collect_exported_type_info_closure(&mut type_exports, sym.type_info.as_ref(), type_layouts);
    }
    for (name, sym) in scope.symbols.iter() {
        if matches!(sym.kind, SymbolKind::DerivedType) {
            collect_exported_type_closure(&mut type_exports, name, type_layouts);
        }
    }
    for (_name, sym) in &procs {
        collect_exported_type_info_closure(&mut type_exports, sym.type_info.as_ref(), type_layouts);
        if let Some(pscope) = st
            .scopes
            .iter()
            .find(|s| {
                s.parent == Some(mod_scope_id)
                    && match &s.kind {
                        ScopeKind::Function(n) | ScopeKind::Subroutine(n) => {
                            n.eq_ignore_ascii_case(&sym.name)
                        }
                        _ => false,
                    }
            })
            .or_else(|| {
                st.scopes.iter().find(|s| {
                    let matches_name = match &s.kind {
                        ScopeKind::Function(n) | ScopeKind::Subroutine(n) => {
                            n.eq_ignore_ascii_case(&sym.name)
                        }
                        _ => false,
                    };
                    if !matches_name {
                        return false;
                    }
                    let Some(parent_id) = s.parent else {
                        return false;
                    };
                    let parent = st.scope(parent_id);
                    matches!(parent.kind, ScopeKind::Interface)
                        && parent.parent == Some(mod_scope_id)
                })
            })
        {
            for arg_name in &pscope.arg_order {
                if let Some(arg_sym) = pscope.symbols.get(&arg_name.to_lowercase()) {
                    collect_exported_type_info_closure(
                        &mut type_exports,
                        arg_sym.type_info.as_ref(),
                        type_layouts,
                    );
                }
            }
        }
    }
    for key in &type_exports {
        if let Some(layout) = type_layouts
            .get_for_scope(mod_scope_id, key)
            .or_else(|| type_layouts.get(key))
        {
            let canonical = type_layouts.canonical_key_for_layout(layout);
            let access = serialized_type_access(scope, layout);
            emit_type(
                &mut out,
                &canonical,
                access,
                type_layouts,
                provider_defaults,
            )?;
        }
    }

    // ---- Interfaces ----
    // Per F2018 §11.2.3, submodules see their parent module's PRIVATE
    // generic interfaces. Emit every NamedInterface (and constructor
    // interfaces represented as DerivedType with non-empty arg_names),
    // tagging private ones with a `private` marker. Importing scopes
    // that use the module without submodule access filter the private
    // entries out via `Symbol::attrs.access == Private` (see
    // SymbolTable::lookup_in_guarded). Without this, a submodule that
    // dispatches a private parent generic emits a bare `bl _<name>`,
    // since the loader-installed scope had no NamedInterface with that
    // name to resolve against.
    let ifaces: Vec<_> = all_syms
        .iter()
        .filter(|(_, sym)| {
            matches!(sym.kind, SymbolKind::NamedInterface)
                || (matches!(sym.kind, SymbolKind::DerivedType) && !sym.arg_names.is_empty())
        })
        .collect();
    for (name, sym) in &ifaces {
        emit_interface(&mut out, name, sym, scope);
    }

    Ok(add_integrity_headers(out))
}

fn is_public(sym: &Symbol, scope: &Scope) -> bool {
    match sym.attrs.access {
        Access::Private => false,
        Access::Public => true,
        Access::Default => !matches!(scope.default_access, Access::Private),
    }
}

fn serialized_type_access(scope: &Scope, layout: &TypeLayout) -> Access {
    scope
        .symbols
        .get(&layout.name.to_ascii_lowercase())
        .filter(|sym| matches!(sym.kind, SymbolKind::DerivedType))
        .map(|sym| {
            if is_public(sym, scope) {
                Access::Public
            } else {
                Access::Private
            }
        })
        // A closure-only layout is implementation support for another
        // exported entity, not an independently exported module name.
        .unwrap_or(Access::Private)
}

fn bound_proc_target_export_key(module_key: &str, target_name: &str) -> String {
    split_module_procedure_link_name(target_name)
        .filter(|(owner, _)| owner.eq_ignore_ascii_case(module_key))
        .map(|(_, procedure)| procedure)
        .unwrap_or(target_name)
        .to_lowercase()
}

fn emit_variable(
    out: &mut String,
    mod_key: &str,
    name: &str,
    sym: &Symbol,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    provider_defaults: DefaultKinds,
) {
    let global_key = (mod_key.to_string(), name.to_lowercase());
    let global_info = globals.get(&global_key);
    let type_str = if matches!(sym.kind, SymbolKind::ProcedurePointer) {
        sym.attrs
            .procedure_iface
            .as_ref()
            .map(|iface| format!("type({})", iface))
            .unwrap_or_else(|| "unknown".to_string())
    } else if let (
        Some(TypeInfo::Character { len: None, kind }),
        Some(ModuleGlobalInfo {
            char_kind: crate::ir::lower::CharKind::Fixed(n),
            ..
        }),
    ) = (sym.type_info.as_ref(), global_info)
    {
        character_type_to_string(Some(*n), Some(kind.unwrap_or(1)))
    } else {
        type_info_to_string(sym.type_info.as_ref(), provider_defaults)
    };
    write!(out, "@var {} : {}", name, type_str).unwrap();

    let mut attrs = Vec::new();
    if sym.attrs.allocatable {
        attrs.push("allocatable");
    }
    if sym.attrs.save {
        attrs.push("save");
    }
    if sym.attrs.pointer {
        attrs.push("pointer");
    }
    if matches!(sym.kind, SymbolKind::ProcedurePointer) {
        attrs.push("procptr");
    }
    if sym.attrs.target {
        attrs.push("target");
    }
    if sym.attrs.asynchronous {
        attrs.push("asynchronous");
    }
    if sym.attrs.contiguous {
        attrs.push("contiguous");
    }
    if sym.attrs.volatile {
        attrs.push("volatile");
    }
    if sym.attrs.protected {
        attrs.push("protected");
    }
    if sym.attrs.openmp_threadprivate {
        attrs.push("threadprivate");
    }
    if sym.attrs.access == Access::Private {
        attrs.push("private");
    }
    if !attrs.is_empty() {
        write!(out, ", {}", attrs.join(", ")).unwrap();
    }

    if let Some(info) = global_info {
        write!(out, " @ir {}", info.symbol).unwrap();
        if info.deferred_char {
            write!(out, " @deferred_char").unwrap();
        }
        if info.declared_rank > 0 {
            write!(out, " @rank {}", info.declared_rank).unwrap();
        }
        if !info.dims.is_empty() {
            write!(out, " @dims").unwrap();
            for (lo, ext) in &info.dims {
                write!(out, " {}:{}", lo, ext).unwrap();
            }
        }
    }
    writeln!(out).unwrap();
}

fn emit_parameter(
    out: &mut String,
    mod_key: &str,
    name: &str,
    sym: &Symbol,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    provider_defaults: DefaultKinds,
) {
    let global_key = (mod_key.to_string(), name.to_lowercase());
    let global_info = globals.get(&global_key);
    let type_str = if let (
        Some(TypeInfo::Character { len: None, kind }),
        Some(ModuleGlobalInfo {
            char_kind: crate::ir::lower::CharKind::Fixed(n),
            ..
        }),
    ) = (sym.type_info.as_ref(), global_info)
    {
        character_type_to_string(Some(*n), Some(kind.unwrap_or(1)))
    } else {
        type_info_to_string(sym.type_info.as_ref(), provider_defaults)
    };
    let is_private = sym.attrs.access == Access::Private;
    let const_char_hex = sym
        .const_char_value
        .as_ref()
        .map(|value| hex_encode_bytes(&crate::source_bytes::from_source_view(value)));
    let const_int = sym
        .const_value
        .or_else(|| global_info.and_then(|info| info.const_value));
    let const_real = global_info.and_then(|info| info.const_real_value);
    if let Some(rv) = const_real {
        let suf = if is_private { ", private" } else { "" };
        let char_suf = const_char_hex
            .as_ref()
            .map(|hex| format!(" @charhex {}", hex))
            .unwrap_or_default();
        writeln!(
            out,
            "@param {} : {} = {:.17e}{}{}",
            name, type_str, rv, suf, char_suf
        )
        .unwrap();
    } else if let Some(cv) = const_int {
        // Place `, private` after the value so parse_var's
        // rfind(" = ") inside type_str continues to work.
        let suf = if is_private { ", private" } else { "" };
        let char_suf = const_char_hex
            .as_ref()
            .map(|hex| format!(" @charhex {}", hex))
            .unwrap_or_default();
        writeln!(
            out,
            "@param {} : {} = {}{}{}",
            name, type_str, cv, suf, char_suf
        )
        .unwrap();
    } else if let Some(info) = global_info {
        // For @ir-backed params, attach `, private` to the type so
        // the parser sees it in attr_str rather than after @ir.
        let type_with_attr = if is_private {
            format!("{}, private", type_str)
        } else {
            type_str
        };
        let char_suf = const_char_hex
            .as_ref()
            .map(|hex| format!(" @charhex {}", hex))
            .unwrap_or_default();
        write!(
            out,
            "@param {} : {} @ir {}",
            name, type_with_attr, info.symbol
        )
        .unwrap();
        if info.deferred_char {
            write!(out, " @deferred_char").unwrap();
        }
        if info.declared_rank > 0 {
            write!(out, " @rank {}", info.declared_rank).unwrap();
        }
        if !info.dims.is_empty() {
            write!(out, " @dims").unwrap();
            for (lo, ext) in &info.dims {
                write!(out, " {}:{}", lo, ext).unwrap();
            }
        }
        writeln!(out, "{}", char_suf).unwrap();
    } else {
        let suf = if is_private { ", private" } else { "" };
        let char_suf = const_char_hex
            .as_ref()
            .map(|hex| format!(" @charhex {}", hex))
            .unwrap_or_default();
        writeln!(out, "@param {} : {}{}{}", name, type_str, suf, char_suf).unwrap();
    }
}

fn procedure_scope_for_module<'a>(
    st: &'a SymbolTable,
    mod_scope_id: ScopeId,
    name: &str,
) -> Option<&'a Scope> {
    st.scopes
        .iter()
        .find(|scope| {
            scope.parent == Some(mod_scope_id)
                && matches!(
                    &scope.kind,
                    ScopeKind::Function(proc_name) | ScopeKind::Subroutine(proc_name)
                        if proc_name.eq_ignore_ascii_case(name)
                )
        })
        .or_else(|| {
            st.scopes.iter().find(|scope| {
                let matches_name = matches!(
                    &scope.kind,
                    ScopeKind::Function(proc_name) | ScopeKind::Subroutine(proc_name)
                        if proc_name.eq_ignore_ascii_case(name)
                );
                if !matches_name {
                    return false;
                }
                let Some(parent_id) = scope.parent else {
                    return false;
                };
                matches!(st.scope(parent_id).kind, ScopeKind::Interface)
                    && st.scope(parent_id).parent == Some(mod_scope_id)
            })
        })
}

fn emit_procedure(
    out: &mut String,
    name: &str,
    sym: &Symbol,
    st: &SymbolTable,
    mod_scope_id: ScopeId,
    ir_module: &IrModule,
    descriptor_params: &HashMap<String, Vec<bool>>,
    char_len_star_params: &HashMap<String, Vec<bool>>,
    provider_defaults: DefaultKinds,
) {
    let is_func = matches!(sym.kind, SymbolKind::Function);
    let kind_str = if is_func { "function" } else { "subroutine" };
    let proc_scope = procedure_scope_for_module(st, mod_scope_id, name);

    if is_func {
        // RESULT identity is semantic state, not something reconstructed from
        // whichever same-typed local happens to sort first.
        let result_var = proc_scope.and_then(Scope::procedure_result_symbol);
        let ret_str = type_info_to_string(sym.type_info.as_ref(), provider_defaults);
        write!(out, "@function {} -> {}", sym.name, ret_str).unwrap();
        if sym.attrs.allocatable {
            write!(out, ", result_allocatable").unwrap();
        }
        if sym.attrs.pointer {
            write!(out, ", result_pointer").unwrap();
        }
        let result_interface_name = result_var
            .filter(|result| result.kind == SymbolKind::ProcedurePointer)
            .and_then(|result| result.attrs.procedure_iface.as_deref());
        if let Some(interface_name) = result_interface_name {
            write!(out, ", result_procedure={interface_name}").unwrap();
        }
        if let Some(import) = result_interface_name.and_then(|interface_name| {
            proc_scope.and_then(|scope| procedure_interface_import(st, scope, interface_name))
        }) {
            write!(
                out,
                ", result_procedure_original={}, result_procedure_module={}, result_procedure_nature={}",
                import.original,
                import.source_module,
                module_nature_token(import.source_nature)
            )
            .unwrap();
        }
        if sym.attrs.result_rank > 0 {
            write!(out, ", result_rank={}", sym.attrs.result_rank).unwrap();
        }
        if let Some(result_name) = proc_scope.and_then(|scope| scope.result_name.as_deref()) {
            if !result_name.eq_ignore_ascii_case(name) {
                write!(out, ", result_name={result_name}").unwrap();
            }
        }
        // Sprint35-SMP Phase 3: serialize the result variable's
        // explicit-shape bounds so split-file submodule bodies (where
        // the body's TU loads the parent module from .amod) can rebuild
        // a same-shape ArraySpec at load time. Without this, the body's
        // `res(i) = …` lowers against an AssumedShape result and the
        // function prologue fails to allocate the runtime-shape buffer.
        if !sym.attrs.allocatable && !sym.attrs.pointer && sym.attrs.result_rank > 0 {
            let bounds = result_var
                .map(|result| &result.attrs.array_spec)
                .and_then(|specs| stringify_array_bounds(specs));
            if let Some(s) = bounds {
                write!(out, ", result_array_bounds=\"{}\"", s).unwrap();
            }
        }
    } else {
        write!(out, "@subroutine {}", sym.name).unwrap();
    }
    if sym.attrs.pure {
        write!(out, ", pure").unwrap();
    }
    if sym.attrs.elemental {
        write!(out, ", elemental").unwrap();
    }
    if sym.attrs.external
        && !sym.attrs.is_separate_module_interface
        && !sym.attrs.is_separate_module_procedure
    {
        write!(out, ", external").unwrap();
    }
    if sym.attrs.abstract_interface {
        write!(out, ", abstract_interface").unwrap();
    }
    if sym.attrs.is_separate_module_interface {
        write!(out, ", module_interface").unwrap();
    }
    if sym.attrs.is_separate_module_procedure {
        write!(out, ", module_procedure").unwrap();
    }
    if !is_public(sym, st.scope(mod_scope_id)) {
        write!(out, ", private").unwrap();
    }
    if sym.attrs.bind_c && sym.attrs.binding_label.is_none() {
        write!(out, ", bind_c").unwrap();
    }
    if let Some(binding_label) = &sym.attrs.binding_label {
        write!(out, ", bind={}", binding_label).unwrap();
    }
    writeln!(out).unwrap();

    let name_lc = name.to_lowercase();
    let link_name = crate::ir::lower::symbol_link_name(st, sym);
    let metadata_name = crate::ir::lower::symbol_abi_metadata_name(st, sym).to_lowercase();
    let ir_func = ir_module
        .functions
        .iter()
        .find(|func| func.name.eq_ignore_ascii_case(&link_name));
    let visible_ir_params: Vec<_> = ir_func
        .map(|func| {
            func.params
                .iter()
                .filter(|param| {
                    param.name != "_sret"
                        && !param.name.starts_with("__len_")
                        && !param.name.starts_with("__host_")
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let is_bind_c = sym.attrs.bind_c;
    let declared_descriptor_params = descriptor_params
        .get(&metadata_name)
        .or_else(|| descriptor_params.get(&name_lc));
    let declared_char_len_star_params = char_len_star_params
        .get(&metadata_name)
        .or_else(|| char_len_star_params.get(&name_lc));
    let hidden_char_len_args: Vec<String> = proc_scope
        .map(|pscope| {
            pscope
                .arg_order
                .iter()
                .enumerate()
                .filter_map(|(arg_idx, arg_name)| {
                    let arg_sym = pscope.symbols.get(&arg_name.to_lowercase())?;
                    let is_assumed_len = declared_char_len_star_params
                        .and_then(|flags| flags.get(arg_idx).copied())
                        .unwrap_or(
                            arg_sym.attrs.assumed_length_character
                                && !arg_sym.attrs.allocatable
                                && !is_bind_c,
                        );
                    is_assumed_len.then(|| arg_name.clone())
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(pscope) = proc_scope {
        for (arg_idx, arg_name) in pscope.arg_order.iter().enumerate() {
            if let Some(arg_sym) = pscope.symbols.get(&arg_name.to_lowercase()) {
                let type_str = type_info_to_string(arg_sym.type_info.as_ref(), provider_defaults);
                write!(out, "  @arg {} : {}", arg_name, type_str).unwrap();
                let mut arg_attrs = Vec::new();
                if let Some(intent) = &arg_sym.attrs.intent {
                    arg_attrs.push(match intent {
                        Intent::In => "intent(in)",
                        Intent::Out => "intent(out)",
                        Intent::InOut => "intent(inout)",
                    });
                }
                if arg_sym.attrs.optional {
                    arg_attrs.push("optional");
                }
                if arg_sym.attrs.value {
                    arg_attrs.push("value");
                }
                let is_descriptor_arg = ir_func
                    .and_then(|_| visible_ir_params.get(arg_idx))
                    .map(|param| {
                        matches!(
                            &param.ty,
                            crate::ir::types::IrType::Ptr(inner)
                                if matches!(
                                    inner.as_ref(),
                                    crate::ir::types::IrType::Array(elem, 392)
                                        if matches!(
                                            elem.as_ref(),
                                            crate::ir::types::IrType::Int(
                                                crate::ir::types::IntWidth::I8
                                            )
                                        )
                                )
                        )
                    })
                    .unwrap_or(false)
                    || declared_descriptor_params
                        .and_then(|flags| flags.get(arg_idx))
                        .copied()
                        .unwrap_or(false)
                    || matches!(
                        arg_sym.type_info,
                        Some(TypeInfo::Class(_)) | Some(TypeInfo::ClassStar)
                    );
                if is_descriptor_arg {
                    arg_attrs.push("descriptor");
                }
                if arg_sym.attrs.allocatable {
                    arg_attrs.push("allocatable");
                }
                if arg_sym.attrs.pointer {
                    arg_attrs.push("pointer");
                }
                if arg_sym.attrs.target {
                    arg_attrs.push("target");
                }
                if arg_sym.attrs.asynchronous {
                    arg_attrs.push("asynchronous");
                }
                if arg_sym.attrs.contiguous {
                    arg_attrs.push("contiguous");
                }
                if arg_sym.attrs.volatile {
                    arg_attrs.push("volatile");
                }
                if arg_sym
                    .attrs
                    .array_spec
                    .iter()
                    .any(|spec| matches!(spec, crate::ast::decl::ArraySpec::AssumedRank))
                {
                    arg_attrs.push("assumed-rank");
                }
                // F2018 §15.4.3.6: a `procedure(iface) :: name` dummy is
                // a procedure formal. The producer side stores this as a
                // Variable with EXTERNAL set; without preserving the flag
                // the consumer-side dispatch can't tell it apart from a
                // data dummy and rejects valid procedure-actual binding
                // (e.g. passing `do_not_select` into LAPACK `gees`).
                if arg_sym.attrs.external {
                    arg_attrs.push("external");
                }
                let proc_iface_attr = arg_sym
                    .attrs
                    .procedure_iface
                    .as_ref()
                    .map(|n| format!("procedure({})", n));
                if let Some(s) = proc_iface_attr.as_ref() {
                    arg_attrs.push(s.as_str());
                }
                let proc_iface_import_attrs = arg_sym
                    .attrs
                    .procedure_iface
                    .as_deref()
                    .and_then(|interface_name| {
                        procedure_interface_import(st, pscope, interface_name)
                    })
                    .map(|import| {
                        vec![
                            format!("procedure_original={}", import.original),
                            format!("procedure_module={}", import.source_module),
                            format!(
                                "procedure_nature={}",
                                module_nature_token(import.source_nature)
                            ),
                        ]
                    });
                if let Some(import_attrs) = proc_iface_import_attrs.as_ref() {
                    arg_attrs.extend(import_attrs.iter().map(String::as_str));
                }
                // Sprint35-SMP Phase 1: emit the dummy's rank so SMP-body
                // synthesis on the consumer side can rebuild a same-rank
                // array_spec without re-walking the AST decls (which only
                // exist on the producer side at .amod write time).
                let rank_attr = format!("rank={}", arg_sym.attrs.array_spec.len());
                if !arg_sym.attrs.array_spec.is_empty() {
                    arg_attrs.push(rank_attr.as_str());
                }
                let array_spec_attr = stringify_dummy_array_spec(&arg_sym.attrs.array_spec)
                    .map(|spec| format!("array_spec=\"{spec}\""));
                if let Some(spec) = array_spec_attr.as_deref() {
                    arg_attrs.push(spec);
                }
                if !arg_attrs.is_empty() {
                    write!(out, ", {}", arg_attrs.join(", ")).unwrap();
                }
                writeln!(out).unwrap();
            } else {
                writeln!(out, "  @arg {}", arg_name).unwrap();
            }
        }
    } else {
        // Fallback: use arg_names from the symbol (no type info).
        for arg_name in &sym.arg_names {
            writeln!(out, "  @arg {}", arg_name).unwrap();
        }
    }

    // Hidden character-length args. Prefer the exact lowering mask because
    // TypeInfo cannot distinguish `len=*` from `len=:` once sema has lowered
    // both to `len: None`; this matters for allocatable assumed-length
    // character-array dummies.
    for arg_name in &hidden_char_len_args {
        writeln!(out, "  @arg {}@len : integer(8)", arg_name).unwrap();
    }

    writeln!(out, "@end {}", kind_str).unwrap();
    writeln!(out).unwrap();
}

fn emit_type(
    out: &mut String,
    name: &str,
    access: Access,
    type_layouts: &TypeLayoutRegistry,
    provider_defaults: DefaultKinds,
) -> Result<(), String> {
    if let Some(layout) = type_layouts.get(&name.to_lowercase()) {
        let access = match access {
            Access::Private => "private",
            Access::Public | Access::Default => "public",
        };
        writeln!(out, "@type {}, {}", layout.name, access).unwrap();
        writeln!(out, "  @layout size={} align={}", layout.size, layout.align).unwrap();
        if let Some(ref parent) = layout.parent {
            writeln!(out, "  @extends {}", parent).unwrap();
        }
        if layout.is_abstract {
            writeln!(out, "  @abstract").unwrap();
        }
        for field in &layout.fields {
            let ft = type_info_to_string(Some(&field.type_info), provider_defaults);
            let dims = if field.dims.is_empty() {
                String::new()
            } else {
                let rendered = field
                    .dims
                    .iter()
                    .map(|(lower, extent)| {
                        let upper = lower + extent - 1;
                        format!("{}:{}", lower, upper)
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!(" @dims {}", rendered)
            };
            let mut attrs = String::new();
            if field.allocatable {
                attrs.push_str(" @allocatable");
            }
            if field.pointer {
                attrs.push_str(" @pointer");
            }
            if field.deferred_char {
                attrs.push_str(" @deferred_char");
            }
            if field.procedure_pointer {
                attrs.push_str(" @procptr");
            }
            if field.procedure_pointer_nopass {
                attrs.push_str(" @nopass");
            }
            if field.target {
                attrs.push_str(" @target");
            }
            if field.declared_array && field.dims.is_empty() {
                attrs.push_str(" @declared_array");
            }
            if let Some(default_init) = &field.default_init {
                attrs.push_str(&render_field_default_init(default_init));
            }
            let access = match field.access {
                Access::Private => "private",
                Access::Public | Access::Default => "public",
            };
            attrs.push_str(&format!(" @access {access}"));
            if let Some(owner_module) = field.owner_module.as_deref() {
                attrs.push_str(&format!(" @owner {owner_module}"));
            }
            writeln!(
                out,
                "  @field {} : {} @offset {} @size {}{}{}",
                field.name, ft, field.offset, field.size, dims, attrs
            )
            .unwrap();
        }
        for bp in &layout.bound_procs {
            let abi_suffix = if bp.abi_name != bp.method_name.to_lowercase() {
                format!(" @abi {}", bp.abi_name)
            } else {
                String::new()
            };
            let pass_suffix = if bp.nopass {
                " @nopass".to_string()
            } else {
                let pass_name = bp.pass_name.as_deref().ok_or_else(|| {
                    format!(
                        "cannot emit .amod binding '{}': passed-object dummy is unresolved",
                        bp.method_name
                    )
                })?;
                let pass_index = bp.pass_index.ok_or_else(|| {
                    format!(
                        "cannot emit .amod binding '{}': passed-object index is unresolved",
                        bp.method_name
                    )
                })?;
                format!(" @pass-name {pass_name} @pass-index {pass_index}")
            };
            if bp.method_name == bp.target_name {
                writeln!(
                    out,
                    "  @binds {}{}{}",
                    bp.method_name, pass_suffix, abi_suffix
                )
                .unwrap();
            } else {
                writeln!(
                    out,
                    "  @binds {} => {}{}{}",
                    bp.method_name, bp.target_name, pass_suffix, abi_suffix
                )
                .unwrap();
            }
        }

        fn render_field_default_init(init: &crate::sema::type_layout::FieldDefaultInit) -> String {
            match init {
                crate::sema::type_layout::FieldDefaultInit::Character(value) => {
                    format!(
                        " @init=charhex:{}",
                        hex_encode_bytes(&crate::source_bytes::from_source_view(value))
                    )
                }
                crate::sema::type_layout::FieldDefaultInit::Integer(value) => {
                    format!(" @init=int:{}", value)
                }
                crate::sema::type_layout::FieldDefaultInit::Logical(value) => {
                    format!(" @init=logical:{}", if *value { "true" } else { "false" })
                }
                crate::sema::type_layout::FieldDefaultInit::Real(value) => {
                    format!(" @init=realbits:{:016x}", value.to_bits())
                }
                crate::sema::type_layout::FieldDefaultInit::Derived(_) => {
                    let encoded = encode_nested_field_default_init(init);
                    format!(" @init=exprhex:{}", hex_encode_bytes(encoded.as_bytes()))
                }
                crate::sema::type_layout::FieldDefaultInit::ProcedurePointer(target) => {
                    format!(" @init=procptr:{}", target)
                }
            }
        }
        for fp in &layout.final_procs {
            writeln!(
                out,
                "  @final {} rank={} elemental={}",
                fp.name, fp.rank, fp.elemental
            )
            .unwrap();
        }
        if let Some(owner_module) = &layout.owner_module {
            writeln!(out, "  @owner {}", owner_module).unwrap();
        }
        writeln!(out, "  @tag {}", layout.type_tag).unwrap();
    }
    writeln!(out, "@end type").unwrap();
    writeln!(out).unwrap();
    Ok(())
}

fn collect_exported_type_info_closure(
    out: &mut BTreeSet<String>,
    info: Option<&TypeInfo>,
    type_layouts: &TypeLayoutRegistry,
) {
    match info {
        Some(TypeInfo::Derived(name)) | Some(TypeInfo::Class(name)) => {
            collect_exported_type_closure(out, name, type_layouts);
        }
        _ => {}
    }
}

fn collect_exported_type_closure(
    out: &mut BTreeSet<String>,
    type_name: &str,
    type_layouts: &TypeLayoutRegistry,
) {
    let key = type_name.to_lowercase();
    if !out.insert(key.clone()) {
        return;
    }
    let Some(layout) = type_layouts.get(&key) else {
        return;
    };
    if let Some(parent) = &layout.parent {
        collect_exported_type_closure(out, parent, type_layouts);
    }
    for field in &layout.fields {
        collect_exported_type_info_closure(out, Some(&field.type_info), type_layouts);
    }
}

fn emit_interface(out: &mut String, name: &str, sym: &Symbol, scope: &Scope) {
    let effective_private = match sym.attrs.access {
        Access::Private => true,
        Access::Public => false,
        Access::Default => matches!(scope.default_access, Access::Private),
    };
    let suf = if effective_private { ", private" } else { "" };
    let interface_name = if matches!(sym.kind, SymbolKind::NamedInterface) {
        sym.name.as_str()
    } else {
        name
    };
    writeln!(out, "@interface {}{}", interface_name, suf).unwrap();
    let mut specifics = sym.arg_names.clone(); // arg_names repurposed for specific list
    specifics.sort();
    for s in &specifics {
        writeln!(out, "  @specific {}", s).unwrap();
    }
    writeln!(out, "@end interface").unwrap();
    writeln!(out).unwrap();
}

// =====================================================================
// Helpers
// =====================================================================

fn type_info_to_string(info: Option<&TypeInfo>, provider_defaults: DefaultKinds) -> String {
    let concrete = info.map(|info| concretize_intrinsic_type_info(info, provider_defaults));
    match concrete.as_ref() {
        Some(TypeInfo::Integer { kind }) => match kind {
            Some(k) => format!("integer({})", k),
            None => "integer".to_string(),
        },
        // l07 flag: enumeration types need full .amod round-trip
        // support when multi-file lands; the string form keeps
        // hashing/diagnostics honest meanwhile.
        Some(TypeInfo::Enumeration(name)) => format!("enumeration({})", name),
        Some(TypeInfo::Real { kind }) => match kind {
            Some(k) => format!("real({})", k),
            None => "real".to_string(),
        },
        Some(TypeInfo::DoublePrecision) => "double precision".to_string(),
        Some(TypeInfo::Complex { kind }) => match kind {
            Some(k) => format!("complex({})", k),
            None => "complex".to_string(),
        },
        Some(TypeInfo::Logical { kind }) => match kind {
            Some(k) => format!("logical({})", k),
            None => "logical".to_string(),
        },
        Some(TypeInfo::Character { len, kind }) => character_type_to_string(*len, *kind),
        Some(TypeInfo::Derived(name)) => format!("type({})", name),
        Some(TypeInfo::Class(name)) => format!("class({})", name),
        Some(TypeInfo::ClassStar) => "class(*)".to_string(),
        Some(TypeInfo::TypeStar) => "type(*)".to_string(),
        None => "unknown".to_string(),
    }
}

fn character_type_to_string(len: Option<i64>, kind: Option<u8>) -> String {
    let len = len.map_or_else(|| ":".to_string(), |len| len.to_string());
    match kind {
        Some(kind) => format!("character(len={len},kind={kind})"),
        None => format!("character(len={len})"),
    }
}

fn fnv1a_hex(content: &str) -> String {
    fnv1a_hex_bytes(content.as_bytes())
}

pub(crate) fn artifact_fingerprint(content: &str) -> String {
    fnv1a_hex(content)
}

fn fnv1a_hex_bytes(content: &[u8]) -> String {
    // FNV-1a 64-bit hash for .amod content fingerprinting.
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in content {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

fn add_integrity_headers(mut content: String) -> String {
    let body_start = content
        .find("\n\n")
        .map(|idx| idx + 2)
        .unwrap_or(content.len());
    let body = &content[body_start..];
    let integrity = format!(
        "# content-length: {}\n# content-checksum: fnv1a:{}\n",
        body.len(),
        fnv1a_hex(body)
    );
    let insert_at = content.find('\n').map(|idx| idx + 1).unwrap_or(0);
    content.insert_str(insert_at, &integrity);
    content
}

fn compile_timestamp() -> String {
    // ISO-8601 timestamp.  For deterministic builds, this could
    // be overridden by an environment variable.
    // TODO: use actual system time.
    "2026-01-01T00:00:00Z".to_string()
}

// =====================================================================
// Reader
// =====================================================================

/// A procedure argument parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct AmodArg {
    pub name: String,
    pub type_info: Option<TypeInfo>,
    pub intent: Option<Intent>,
    pub optional: bool,
    pub value: bool,
    pub descriptor: bool,
    pub allocatable: bool,
    pub pointer: bool,
    pub target: bool,
    pub asynchronous: bool,
    pub contiguous: bool,
    pub volatile: bool,
    pub hidden: bool,
    /// True for `procedure(iface) :: name` dummies. The producer side
    /// stores these as Variable + EXTERNAL; the consumer-side dispatch
    /// uses this flag to identify procedure formals and skip the data
    /// type-matching that would otherwise reject procedure-actual
    /// binding (the .amod writer normalizes the type to the interface's
    /// return type).
    pub external: bool,
    /// For procedure dummy args (`procedure(iface) :: name`), the
    /// interface name. Without this the consumer side can't resolve
    /// the dummy to its abstract interface and falls back to emitting
    /// the dummy name as an external symbol — see the SGGES3 / selctg
    /// failure in stdlib_lapack_eigv_gen.
    pub procedure_iface: Option<String>,
    /// Procedure-local USE association that supplies `procedure_iface`.
    /// Keeping this on the dummy avoids hoisting a signature dependency to
    /// module scope, where a public-default module could re-export it.
    pub procedure_iface_import: Option<UseRename>,
    /// True when the source dummy used DIMENSION(..). Rank alone cannot
    /// distinguish assumed-rank from a rank-one assumed-shape dummy.
    pub assumed_rank: bool,
    /// Sprint35-SMP Phase 1: rank of the dummy (number of array dimensions);
    /// 0 for scalar. When non-zero the loader reconstructs a SymbolAttrs
    /// `array_spec` of this rank, deriving each dim's kind from the
    /// `descriptor` / `allocatable` / `pointer` flags when exact metadata
    /// from a current `.amod` is unavailable.
    pub rank: u8,
    /// Exact source dummy shape. Procedure characteristics include shape and
    /// nonconstant bound dependence, so rank-only reconstruction is not
    /// semantically sufficient.
    pub array_spec: Option<Vec<crate::ast::decl::ArraySpec>>,
}

/// A procedure parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct AmodProc {
    pub name: String,
    pub kind: SymbolKind,
    pub return_type: Option<TypeInfo>,
    pub result_allocatable: bool,
    pub result_pointer: bool,
    /// Declared interface of a procedure-pointer function result. A plain
    /// `result_pointer` is insufficient because data pointers can also have
    /// a derived declared type.
    pub result_procedure_iface: Option<String>,
    /// Procedure-local USE association that supplies the declared interface
    /// for a procedure-pointer function result.
    pub result_procedure_iface_import: Option<UseRename>,
    pub result_rank: u8,
    /// Sprint35-SMP Phase 2: the result variable's user-declared name
    /// (from `result(X)` clause). None when the result name matches
    /// the function name. The submodule body lowering needs this to
    /// resolve `X = ...` assignments inside an SMP body when the body
    /// references the result by its declared name rather than by the
    /// function name.
    pub result_name: Option<String>,
    /// Stringified explicit-shape bounds for the result variable.
    /// `(b1; b2; ...)` per dim, where each is `lower:upper` or just
    /// `upper`. Preserves runtime-shape result sizing across split-file
    /// submodule compilation: SMP body lowering needs `Explicit { upper:
    /// Name(dummy) }` to allocate the result in the prologue. None for
    /// scalar / allocatable / pointer / non-runtime-shape results.
    pub result_array_bounds: Option<String>,
    pub pure: bool,
    pub elemental: bool,
    /// The declaration is an explicit interface for an external procedure,
    /// not a procedure implemented by the module that publishes this file.
    pub external: bool,
    /// The declaration came from an ABSTRACT INTERFACE body and cannot be
    /// used as a callable designator or procedure-pointer target.
    pub abstract_interface: bool,
    pub is_separate_module_interface: bool,
    pub is_separate_module_procedure: bool,
    pub access: Access,
    pub bind_c: bool,
    pub binding_label: Option<String>,
    pub args: Vec<AmodArg>,
}

/// A variable or parameter parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct AmodVar {
    pub name: String,
    pub type_info: Option<TypeInfo>,
    pub is_parameter: bool,
    pub allocatable: bool,
    pub save: bool,
    pub pointer: bool,
    pub proc_pointer: bool,
    pub target: bool,
    pub asynchronous: bool,
    pub contiguous: bool,
    pub volatile: bool,
    pub protected: bool,
    pub openmp_threadprivate: bool,
    pub ir_symbol: Option<String>,
    pub deferred_char: bool,
    pub rank: usize,
    pub dims: Vec<(i64, i64)>,
    pub const_value: Option<i128>,
    pub const_real_value: Option<f64>,
    pub const_char_value: Option<String>,
    /// Access level. F2008 §11.2.3 requires private parent symbols to
    /// be visible in submodules, so the writer emits private entries
    /// with a `private` attribute and the loader honors them via
    /// host association without exposing them to ordinary USE.
    pub access: Access,
}

/// A generic named interface parsed from an .amod file. Each entry
/// maps the interface name (e.g. `add`) to the ordered list of
/// specific procedure names it dispatches to. Used by importing
/// compilation units to reconstruct a `NamedInterface` symbol so
/// generic resolution works across .amod boundaries.
#[derive(Debug, Clone)]
pub struct AmodInterface {
    pub name: String,
    pub specifics: Vec<String>,
    pub access: Access,
}

/// A serialized derived-type layout and the accessibility of its module
/// symbol. Layout presence and USE visibility are deliberately separate:
/// descendant submodules need private layouts, while ordinary consumers must
/// not acquire the private type name.
#[derive(Debug, Clone)]
pub struct AmodType {
    pub layout: TypeLayout,
    pub access: Access,
}

/// One serialized USE binding. `local` is the name visible in this module,
/// `original` is the provider name, and `source_module` identifies the edge.
/// The same shape represents an entry on an ONLY edge or a rename on a bare
/// edge; `ModuleInterface` keeps those categories separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseRename {
    pub local: String,
    pub original: String,
    pub source_module: String,
    pub source_nature: UseNature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmodModuleDependency {
    pub module_name: String,
    pub nature: UseNature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmodHostAssociation {
    All,
    None,
    Only(Vec<String>),
}

/// Complete module interface parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct ModuleInterface {
    pub module_name: String,
    pub default_access: Access,
    pub named_access: Vec<(String, Access)>,
    pub submodule_ancestor: Option<String>,
    pub submodule_parent: Option<String>,
    pub host_association: AmodHostAssociation,
    pub dependencies: Vec<AmodModuleDependency>,
    pub only_imports: Vec<UseRename>,
    pub renames: Vec<UseRename>,
    pub variables: Vec<AmodVar>,
    pub procedures: Vec<AmodProc>,
    pub types: Vec<AmodType>,
    pub interfaces: Vec<AmodInterface>,
    /// F2023 strict enumeration types: (type name, enumerators in
    /// declaration order, access). Ordinals are positional (1-based).
    pub enum_types: Vec<(String, Vec<String>, Access)>,
    pub checksum: Option<String>,
}

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::SystemTime;

thread_local! {
    /// In-memory cache of parsed `.amod` files for the current build.
    /// A second `USE foo` in the same compilation skips the parse step
    /// when the file's mtime hasn't moved since the first access.
    /// Keyed by canonicalized path so two different relative paths
    /// pointing at the same file share the entry. mtime is checked
    /// per access so on-disk edits during a build are picked up.
    static AMOD_CACHE: RefCell<HashMap<PathBuf, (SystemTime, ModuleInterface)>> =
        RefCell::new(HashMap::new());

    /// Counter for cache hits — useful for tests verifying the cache
    /// fires on a second USE of the same module.
    pub static AMOD_CACHE_HITS: RefCell<u64> = const { RefCell::new(0) };
}

/// Read a `.amod` file and return the parsed module interface. The
/// result is cached per-thread, keyed by canonical path + mtime.
pub fn read_amod(path: &Path) -> Result<ModuleInterface, String> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mtime = std::fs::metadata(&canonical)
        .and_then(|m| m.modified())
        .ok();

    if let Some(now) = mtime {
        let cached = AMOD_CACHE.with(|c| {
            c.borrow().get(&canonical).and_then(|(stored, iface)| {
                if *stored == now {
                    Some(iface.clone())
                } else {
                    None
                }
            })
        });
        if let Some(iface) = cached {
            AMOD_CACHE_HITS.with(|h| *h.borrow_mut() += 1);
            return Ok(iface);
        }
    }

    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let iface = read_amod_content(&content, path)?;

    if let Some(stored) = mtime {
        AMOD_CACHE.with(|c| {
            c.borrow_mut()
                .insert(canonical.clone(), (stored, iface.clone()));
        });
    }

    Ok(iface)
}

pub(crate) fn read_amod_content(content: &str, path: &Path) -> Result<ModuleInterface, String> {
    validate_amod_integrity(content, path)?;
    let version = amod_version(content, path)?;
    if version != AMOD_VERSION {
        return Err(format!(
            "{}: incompatible .amod version {} (compiler requires {}); rebuild the provider module",
            path.display(),
            version,
            AMOD_VERSION
        ));
    }
    parse_amod(content, path)
}

fn amod_version(content: &str, path: &Path) -> Result<u32, String> {
    let magic = content
        .lines()
        .next()
        .ok_or_else(|| format!("{}: empty .amod file", path.display()))?;
    let encoded = magic.strip_prefix("#!amod ").ok_or_else(|| {
        format!(
            "{}: not an .amod file (missing #!amod magic)",
            path.display()
        )
    })?;
    encoded
        .trim()
        .parse()
        .map_err(|_| format!("{}: invalid .amod version", path.display()))
}

fn validate_amod_integrity(content: &str, path: &Path) -> Result<(), String> {
    amod_version(content, path)?;

    let body_start = content.find("\n\n").map(|idx| idx + 2).ok_or_else(|| {
        format!(
            "{}: corrupt .amod file (missing header terminator); rebuild the provider module",
            path.display()
        )
    })?;
    let header = &content[..body_start];
    let body = &content[body_start..];

    let mut expected_len: Option<usize> = None;
    let mut expected_checksum: Option<&str> = None;
    for line in header.lines() {
        if let Some(value) = line.strip_prefix("# content-length: ") {
            expected_len = Some(value.trim().parse::<usize>().map_err(|_| {
                format!(
                    "{}: corrupt .amod file (invalid content-length); rebuild the provider module",
                    path.display()
                )
            })?);
        } else if let Some(value) = line.strip_prefix("# content-checksum: fnv1a:") {
            expected_checksum = Some(value.trim());
        }
    }

    let expected_len = expected_len.ok_or_else(|| {
        format!(
            "{}: corrupt .amod file (missing content-length); rebuild the provider module",
            path.display()
        )
    })?;
    let expected_checksum = expected_checksum.ok_or_else(|| {
        format!(
            "{}: corrupt .amod file (missing content-checksum); rebuild the provider module",
            path.display()
        )
    })?;

    let actual_len = body.len();
    if actual_len != expected_len {
        return Err(format!(
            "{}: corrupt .amod file (content length {}, expected {}); rebuild the provider module",
            path.display(),
            actual_len,
            expected_len
        ));
    }

    let actual_checksum = fnv1a_hex(body);
    if actual_checksum != expected_checksum {
        return Err(format!(
            "{}: corrupt .amod file (content checksum {}, expected {}); rebuild the provider module",
            path.display(),
            actual_checksum,
            expected_checksum
        ));
    }

    Ok(())
}

/// Clear the per-thread amod cache. Tests use this to start fresh
/// between cases.
pub fn clear_amod_cache() {
    AMOD_CACHE.with(|c| c.borrow_mut().clear());
    AMOD_CACHE_HITS.with(|h| *h.borrow_mut() = 0);
}

fn parse_use_binding(rest: &str, directive: &str, path: &Path) -> Result<UseRename, String> {
    let malformed = || {
        format!(
            "{}: corrupt .amod file (malformed {} record); rebuild the provider module",
            path.display(),
            directive
        )
    };
    let (lhs, source_module) = rest.split_once(" from ").ok_or_else(malformed)?;
    let (local, original) = lhs.split_once(" = ").ok_or_else(malformed)?;
    let local = local.trim();
    let original = original.trim();
    let source = parse_module_reference(source_module.trim(), directive, path)?;
    if local.is_empty() || original.is_empty() {
        return Err(malformed());
    }
    Ok(UseRename {
        local: local.to_string(),
        original: original.to_string(),
        source_module: source.module_name,
        source_nature: source.nature,
    })
}

fn parse_module_reference(
    value: &str,
    directive: &str,
    path: &Path,
) -> Result<AmodModuleDependency, String> {
    let malformed = || {
        format!(
            "{}: corrupt .amod file (malformed {} module reference); rebuild the provider module",
            path.display(),
            directive
        )
    };
    let (nature, module_name) = match value.split_once(" :: ") {
        Some(("intrinsic", module_name)) => (UseNature::Intrinsic, module_name.trim()),
        Some(("non_intrinsic", module_name)) => (UseNature::NonIntrinsic, module_name.trim()),
        Some(_) => return Err(malformed()),
        None => (UseNature::Normal, value.trim()),
    };
    if module_name.is_empty() {
        return Err(malformed());
    }
    Ok(AmodModuleDependency {
        module_name: module_name.to_string(),
        nature,
    })
}

fn parse_host_association(value: &str, path: &Path) -> Result<AmodHostAssociation, String> {
    let mut parts = value.split_whitespace();
    let policy = match parts.next() {
        Some("all") if parts.next().is_none() => AmodHostAssociation::All,
        Some("none") if parts.next().is_none() => AmodHostAssociation::None,
        Some("only") => {
            let mut names: Vec<String> = parts.map(str::to_ascii_lowercase).collect();
            names.sort_unstable();
            names.dedup();
            AmodHostAssociation::Only(names)
        }
        _ => {
            return Err(format!(
                "{}: corrupt .amod file (malformed host-association header); rebuild the provider module",
                path.display()
            ));
        }
    };
    Ok(policy)
}

fn parse_amod(content: &str, path: &Path) -> Result<ModuleInterface, String> {
    let mut lines = content.lines().peekable();

    // Header: #!amod N
    lines.next().ok_or("empty .amod file")?;
    let version = amod_version(content, path)?;
    if version > AMOD_VERSION {
        eprintln!("warning: {}: .amod version {} is newer than this compiler supports; some information may be ignored", path.display(), version);
    }

    let mut module_name = String::new();
    let mut default_access = None;
    let mut submodule_ancestor = None;
    let mut submodule_parent = None;
    let mut host_association = AmodHostAssociation::All;
    let mut checksum = None;

    // Parse # key: value header lines.
    while let Some(line) = lines.peek() {
        if let Some(rest) = line.strip_prefix("# ") {
            if let Some((key, val)) = rest.split_once(": ") {
                match key {
                    "module" => module_name = val.trim().to_string(),
                    "default-access" => {
                        default_access = Some(match val.trim() {
                            "public" => Access::Public,
                            "private" => Access::Private,
                            _ => {
                                return Err(format!(
                                    "{}: corrupt .amod file (malformed default-access header); rebuild the provider module",
                                    path.display()
                                ));
                            }
                        });
                    }
                    "ancestor-module" => submodule_ancestor = Some(val.trim().to_string()),
                    "parent-submodule" => submodule_parent = Some(val.trim().to_string()),
                    "host-association" => {
                        host_association = parse_host_association(val.trim(), path)?
                    }
                    "checksum" => checksum = Some(val.trim().to_string()),
                    _ => {} // skip other metadata
                }
            }
            lines.next();
        } else if line.is_empty() {
            lines.next();
        } else {
            break;
        }
    }

    if module_name.is_empty() {
        return Err(format!("{}: missing # module: header", path.display()));
    }
    let default_access = match default_access {
        Some(access) => access,
        None if version >= AMOD_DEFAULT_ACCESS_VERSION => {
            return Err(format!(
                "{}: corrupt .amod file (missing default-access header); rebuild the provider module",
                path.display()
            ));
        }
        None => Access::Public,
    };

    let mut dependencies = Vec::new();
    let mut named_access = Vec::new();
    let mut only_imports: Vec<UseRename> = Vec::new();
    let mut renames: Vec<UseRename> = Vec::new();
    let mut variables = Vec::new();
    let mut procedures = Vec::new();
    let mut types = Vec::new();
    let mut interfaces = Vec::new();
    let mut enum_types: Vec<(String, Vec<String>, Access)> = Vec::new();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(dep) = trimmed.strip_prefix("@uses ") {
            dependencies.push(parse_module_reference(dep.trim(), "@uses", path)?);
        } else if let Some(rest) = trimmed.strip_prefix("@access ") {
            let Some((access, name)) = rest.split_once(" :: ") else {
                return Err(format!(
                    "{}: corrupt .amod file (malformed @access record); rebuild the provider module",
                    path.display()
                ));
            };
            let access = match access.trim() {
                "public" => Access::Public,
                "private" => Access::Private,
                _ => {
                    return Err(format!(
                        "{}: corrupt .amod file (malformed @access accessibility); rebuild the provider module",
                        path.display()
                    ));
                }
            };
            let name = name.trim();
            if name.is_empty() || name.split_whitespace().count() != 1 {
                return Err(format!(
                    "{}: corrupt .amod file (malformed @access name); rebuild the provider module",
                    path.display()
                ));
            }
            named_access.push((name.to_ascii_lowercase(), access));
        } else if trimmed == "@use_only" {
            only_imports.push(parse_use_binding("", "@use_only", path)?);
        } else if let Some(rest) = trimmed.strip_prefix("@use_only ") {
            // `@use_only <local> = <original> from <module>`
            only_imports.push(parse_use_binding(rest, "@use_only", path)?);
        } else if trimmed == "@use_rename" {
            renames.push(parse_use_binding("", "@use_rename", path)?);
        } else if let Some(rest) = trimmed.strip_prefix("@use_rename ") {
            // `@use_rename <local> = <original> from <module>`
            renames.push(parse_use_binding(rest, "@use_rename", path)?);
        } else if trimmed.starts_with("@var ") {
            variables.push(parse_var(trimmed, false, version, path)?);
        } else if trimmed.starts_with("@param ") {
            variables.push(parse_var(
                &trimmed.replacen("@param", "@var", 1),
                true,
                version,
                path,
            )?);
        } else if trimmed.starts_with("@function ") || trimmed.starts_with("@subroutine ") {
            let proc = parse_proc(trimmed, &mut lines, version, path)?;
            procedures.push(proc);
        } else if let Some(rest) = trimmed.strip_prefix("@enumtype ") {
            // `@enumtype <public|private> <name> <e1> <e2> ...`
            let mut it = rest.split_whitespace();
            let access = match it.next() {
                Some("public") => Access::Public,
                Some("private") => Access::Private,
                Some(_) | None if version >= 19 => {
                    return Err(format!(
                        "{}: corrupt .amod file (malformed @enumtype access); rebuild the provider module",
                        path.display()
                    ));
                }
                _ => Access::Public,
            };
            let name = it.next();
            let enums: Vec<String> = it.map(str::to_string).collect();
            if version >= 19 {
                let Some(name) = name else {
                    return Err(format!(
                        "{}: corrupt .amod file (missing @enumtype name); rebuild the provider module",
                        path.display()
                    ));
                };
                if enums.is_empty() {
                    return Err(format!(
                        "{}: corrupt .amod file (@enumtype '{}' has no enumerators); rebuild the provider module",
                        path.display(),
                        name
                    ));
                }
                let mut unique = HashSet::new();
                if let Some(duplicate) = enums
                    .iter()
                    .map(|enumerator| enumerator.to_ascii_lowercase())
                    .find(|enumerator| !unique.insert(enumerator.clone()))
                {
                    return Err(format!(
                        "{}: corrupt .amod file (@enumtype '{}' repeats enumerator '{}'); rebuild the provider module",
                        path.display(),
                        name,
                        duplicate
                    ));
                }
                enum_types.push((name.to_string(), enums, access));
            } else if let Some(name) = name.filter(|_| !enums.is_empty()) {
                enum_types.push((name.to_string(), enums, access));
            }
        } else if trimmed.starts_with("@type ") {
            types.push(parse_amod_type(trimmed, &mut lines, version, path)?);
        } else if let Some(name) = trimmed.strip_prefix("@interface ") {
            // Generic interface block: header is `@interface <name>[, private]`,
            // body lists `@specific <proc>` until `@end interface`.
            let header = name.trim();
            let header_parts = split_attrs_top_level(header);
            let malformed_header = header_parts.is_empty()
                || header_parts[0].trim().is_empty()
                || header_parts.len() > 2
                || (header_parts.len() == 2 && header_parts[1] != "private");
            if version >= 19 && malformed_header {
                return Err(format!(
                    "{}: corrupt .amod file (malformed @interface header); rebuild the provider module",
                    path.display()
                ));
            }
            let raw_name = header_parts.first().map_or(header, String::as_str).trim();
            let iface_name = crate::ast::canonical_generic_spec_name(raw_name);
            let access = if header_parts.get(1).is_some_and(|attr| attr == "private") {
                Access::Private
            } else {
                Access::Public
            };
            let mut specifics = Vec::new();
            let mut saw_end = false;
            for iline in lines.by_ref() {
                let t = iline.trim();
                if t == "@end interface" {
                    saw_end = true;
                    break;
                }
                if let Some(spec) = t.strip_prefix("@specific ") {
                    let spec = spec.trim();
                    if version >= 19 && (spec.is_empty() || spec.split_whitespace().count() != 1) {
                        return Err(format!(
                            "{}: corrupt .amod file (malformed @specific in interface '{}'); rebuild the provider module",
                            path.display(),
                            iface_name
                        ));
                    }
                    specifics.push(spec.to_string());
                } else if version >= 19 {
                    return Err(format!(
                        "{}: corrupt .amod file (unknown interface record '{}' in '{}'); rebuild the provider module",
                        path.display(),
                        t,
                        iface_name
                    ));
                }
            }
            if version >= 19 && !saw_end {
                return Err(format!(
                    "{}: corrupt .amod file (unterminated @interface '{}'); rebuild the provider module",
                    path.display(),
                    iface_name
                ));
            }
            if version >= 19 && specifics.is_empty() {
                return Err(format!(
                    "{}: corrupt .amod file (@interface '{}' has no specifics); rebuild the provider module",
                    path.display(),
                    iface_name
                ));
            }
            if version >= 19 {
                let mut unique = HashSet::new();
                if let Some(duplicate) = specifics
                    .iter()
                    .map(|specific| specific.to_ascii_lowercase())
                    .find(|specific| !unique.insert(specific.clone()))
                {
                    return Err(format!(
                        "{}: corrupt .amod file (@interface '{}' repeats specific '{}'); rebuild the provider module",
                        path.display(),
                        iface_name,
                        duplicate
                    ));
                }
            }
            interfaces.push(AmodInterface {
                name: iface_name,
                specifics,
                access,
            });
        }
        // Skip unrecognized directives (forward compatibility).
    }

    validate_serialized_module_procedure_targets(&types, path)?;

    for amod_type in &mut types {
        let layout = &mut amod_type.layout;
        for final_proc in &mut layout.final_procs {
            let source_name = split_module_procedure_link_name(&final_proc.name)
                .filter(|(owner, _)| owner.eq_ignore_ascii_case(&module_name))
                .map(|(_, procedure)| procedure)
                .or_else(|| {
                    (version < 18).then(|| {
                        let legacy_prefix =
                            format!("afs_modproc_{}_", module_name.to_ascii_lowercase());
                        final_proc
                            .name
                            .strip_prefix(&legacy_prefix)
                            .unwrap_or(&final_proc.name)
                    })
                })
                .unwrap_or(&final_proc.name);
            let procedure = procedures
                .iter()
                .find(|proc| proc.name.eq_ignore_ascii_case(source_name));
            if final_proc.rank == usize::MAX {
                let Some(procedure) = procedure else {
                    return Err(format!(
                        "{}: cannot infer rank for legacy @final {}",
                        path.display(),
                        final_proc.name
                    ));
                };
                final_proc.rank = procedure.args.first().map_or(0, |arg| arg.rank as usize);
            }
            if version < AMOD_FINAL_ELEMENTAL_VERSION {
                final_proc.elemental = procedure.is_some_and(|procedure| procedure.elemental);
            }
        }
    }

    if version >= 17 {
        validate_bound_proc_metadata(&types, &procedures, &module_name, path)?;
    }

    Ok(ModuleInterface {
        module_name,
        default_access,
        named_access,
        submodule_ancestor,
        submodule_parent,
        host_association,
        dependencies,
        only_imports,
        renames,
        variables,
        procedures,
        types,
        interfaces,
        enum_types,
        checksum,
    })
}

fn validate_serialized_module_procedure_targets(
    types: &[AmodType],
    path: &Path,
) -> Result<(), String> {
    fn validate(path: &Path, record: &str, target: &str) -> Result<(), String> {
        if is_reserved_module_procedure_link_name(target)
            && split_module_procedure_link_name(target).is_none()
        {
            return Err(format!(
                "{}: corrupt .amod file (malformed encoded module-procedure target '{}' in {}); rebuild the provider module",
                path.display(),
                target,
                record
            ));
        }
        Ok(())
    }

    for amod_type in types {
        for binding in &amod_type.layout.bound_procs {
            validate(path, "@binds", &binding.target_name)?;
        }
        for final_proc in &amod_type.layout.final_procs {
            validate(path, "@final", &final_proc.name)?;
        }
        for field in &amod_type.layout.fields {
            if let Some(crate::sema::type_layout::FieldDefaultInit::ProcedurePointer(target)) =
                &field.default_init
            {
                validate(path, "@init=procptr", target)?;
            }
        }
    }
    Ok(())
}

fn validate_bound_proc_metadata(
    types: &[AmodType],
    procedures: &[AmodProc],
    module_name: &str,
    path: &Path,
) -> Result<(), String> {
    fn same_optional_name(left: Option<&str>, right: Option<&str>) -> bool {
        match (left, right) {
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            (None, None) => true,
            _ => false,
        }
    }

    fn same_binding(left: &BoundProc, right: &BoundProc) -> bool {
        left.method_name.eq_ignore_ascii_case(&right.method_name)
            && left.target_name.eq_ignore_ascii_case(&right.target_name)
            && left.abi_name.eq_ignore_ascii_case(&right.abi_name)
            && left.nopass == right.nopass
            && same_optional_name(left.pass_name.as_deref(), right.pass_name.as_deref())
            && left.pass_index == right.pass_index
    }

    fn type_ref_parts(name: &str) -> (Option<&str>, &str) {
        name.rsplit_once("::")
            .map_or((None, name), |(owner, local)| (Some(owner), local))
    }

    fn type_ref_matches_layout(name: &str, layout: &TypeLayout) -> bool {
        let (owner, local) = type_ref_parts(name);
        local.eq_ignore_ascii_case(&layout.name)
            && owner.is_none_or(|owner| {
                layout
                    .owner_module
                    .as_deref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(owner))
            })
    }

    fn find_type_for_ref<'a>(
        types: &'a [AmodType],
        name: &str,
        fallback_owner: Option<&str>,
    ) -> Option<&'a AmodType> {
        let (owner, local) = type_ref_parts(name);
        let expected_owner = owner.or(fallback_owner);
        types.iter().find(|candidate| {
            candidate.layout.name.eq_ignore_ascii_case(local)
                && expected_owner.is_none_or(|expected| {
                    candidate
                        .layout
                        .owner_module
                        .as_deref()
                        .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
                })
        })
    }

    fn binding_defining_type<'a>(
        types: &'a [AmodType],
        carrying_type: &'a AmodType,
        binding: &BoundProc,
    ) -> (&'a AmodType, bool) {
        let mut defining_type = carrying_type;
        let mut inherited = false;
        let mut seen = HashSet::new();
        loop {
            let key = format!(
                "{}::{}",
                defining_type.layout.owner_module.as_deref().unwrap_or(""),
                defining_type.layout.name
            )
            .to_ascii_lowercase();
            if !seen.insert(key) {
                break;
            }
            let Some(parent_name) = defining_type.layout.parent.as_deref() else {
                break;
            };
            let Some(parent_type) = find_type_for_ref(
                types,
                parent_name,
                defining_type.layout.owner_module.as_deref(),
            ) else {
                break;
            };
            if !parent_type
                .layout
                .bound_procs
                .iter()
                .any(|parent_binding| same_binding(parent_binding, binding))
            {
                break;
            }
            defining_type = parent_type;
            inherited = true;
        }
        (defining_type, inherited)
    }

    fn target_matches_procedure(
        binding: &BoundProc,
        defining_type: &TypeLayout,
        procedure: &AmodProc,
    ) -> bool {
        if binding.target_name.eq_ignore_ascii_case(&procedure.name) {
            return true;
        }
        defining_type.owner_module.as_deref().is_some_and(|owner| {
            split_module_procedure_link_name(&binding.target_name).is_some_and(
                |(target_owner, target_procedure)| {
                    target_owner.eq_ignore_ascii_case(owner)
                        && target_procedure.eq_ignore_ascii_case(&procedure.name)
                },
            )
        })
    }

    for amod_type in types {
        if amod_type
            .layout
            .owner_module
            .as_deref()
            .is_some_and(|owner| !owner.eq_ignore_ascii_case(module_name))
        {
            // Imported ancestor layouts are redundant closure records. Their
            // locally declared bindings were validated in the owning module.
            continue;
        }
        for binding in &amod_type.layout.bound_procs {
            if binding.nopass {
                continue;
            }
            let (defining_type, inherited) = binding_defining_type(types, amod_type, binding);
            if inherited
                && !same_optional_name(
                    defining_type.layout.owner_module.as_deref(),
                    amod_type.layout.owner_module.as_deref(),
                )
            {
                // This binding was inherited from a type owned by another
                // module. Its procedure contract was validated when that
                // provider published its own artifact. Do not correlate it
                // with this module's procedure table: a local procedure may
                // legitimately have the same source or ABI name.
                continue;
            }
            let pass_name = binding.pass_name.as_deref().ok_or_else(|| {
                format!(
                    "{}: corrupt .amod file (missing passed-object name for binding '{}'); rebuild the provider module",
                    path.display(),
                    binding.method_name
                )
            })?;
            let pass_index = binding.pass_index.ok_or_else(|| {
                format!(
                    "{}: corrupt .amod file (missing passed-object index for binding '{}'); rebuild the provider module",
                    path.display(),
                    binding.method_name
                )
            })?;
            let procedure = procedures
                .iter()
                .find(|procedure| {
                    target_matches_procedure(binding, &defining_type.layout, procedure)
                })
                .or_else(|| {
                    procedures
                        .iter()
                        .find(|procedure| procedure.name.eq_ignore_ascii_case(&binding.abi_name))
                });
            let Some(procedure) = procedure else {
                return Err(format!(
                    "{}: corrupt .amod file (missing procedure metadata for binding '{}'); rebuild the provider module",
                    path.display(),
                    binding.method_name
                ));
            };
            let visible_args: Vec<_> = procedure.args.iter().filter(|arg| !arg.hidden).collect();
            let Some(argument) = visible_args.get(pass_index) else {
                return Err(format!(
                    "{}: corrupt .amod file (passed-object index {} is out of range for binding '{}'); rebuild the provider module",
                    path.display(),
                    pass_index,
                    binding.method_name
                ));
            };
            if !argument.name.eq_ignore_ascii_case(pass_name) {
                return Err(format!(
                    "{}: corrupt .amod file (passed-object name/index disagree for binding '{}'); rebuild the provider module",
                    path.display(),
                    binding.method_name
                ));
            }
            if argument.rank != 0 {
                return Err(format!(
                    "{}: corrupt .amod file (passed-object dummy for binding '{}' is not scalar); rebuild the provider module",
                    path.display(),
                    binding.method_name
                ));
            }
            if argument.pointer {
                return Err(format!(
                    "{}: corrupt .amod file (passed-object dummy for binding '{}' has the POINTER attribute); rebuild the provider module",
                    path.display(),
                    binding.method_name
                ));
            }
            if argument.allocatable {
                return Err(format!(
                    "{}: corrupt .amod file (passed-object dummy for binding '{}' has the ALLOCATABLE attribute); rebuild the provider module",
                    path.display(),
                    binding.method_name
                ));
            }
            if argument.value {
                return Err(format!(
                    "{}: corrupt .amod file (passed-object dummy for binding '{}' has the VALUE attribute); rebuild the provider module",
                    path.display(),
                    binding.method_name
                ));
            }
            let has_required_type = matches!(
                argument.type_info.as_ref(),
                Some(TypeInfo::Class(dummy_type))
                    if type_ref_matches_layout(dummy_type, &defining_type.layout)
            );
            if !has_required_type {
                return Err(format!(
                    "{}: corrupt .amod file (passed-object dummy for binding '{}' is not CLASS({})); rebuild the provider module",
                    path.display(),
                    binding.method_name,
                    defining_type.layout.name
                ));
            }
        }
    }
    Ok(())
}

fn parse_var(line: &str, is_param: bool, version: u32, path: &Path) -> Result<AmodVar, String> {
    // @var name : type[, attrs...] [@ir symbol] [@charhex hex] [@deferred_char] [@dims ...]
    let rest = line.strip_prefix("@var ").unwrap_or(line);
    let (name_type, meta_part) = if let Some(idx) = rest.find(" @") {
        (&rest[..idx], Some(&rest[idx + 1..]))
    } else {
        (rest, None)
    };

    let (name, type_and_attrs) = name_type.split_once(" : ").ok_or_else(|| {
        format!(
            "{}: corrupt .amod file (malformed variable record); rebuild the provider module",
            path.display()
        )
    })?;
    let name = name.trim().to_string();

    // Split type from attrs on comma.
    let (type_str, attr_str) = if let Some(idx) = type_and_attrs.find(", ") {
        (&type_and_attrs[..idx], &type_and_attrs[idx + 2..])
    } else {
        (type_and_attrs, "")
    };

    let mut encoded_value = None;
    let mut const_char_value = None;
    // For @param with `= value`, strip the value suffix from the
    // type string before parsing the type.
    let clean_type_str = if is_param {
        if let Some(eq_idx) = type_str.rfind(" = ") {
            encoded_value = Some(type_str[eq_idx + 3..].trim());
            &type_str[..eq_idx]
        } else {
            type_str
        }
    } else {
        type_str
    };

    let type_info = parse_type_info_for_version(
        clean_type_str.trim(),
        version,
        path,
        if is_param { "parameter" } else { "variable" },
    )?;
    let mut const_value = None;
    let mut const_real_value = None;
    if let Some(value) = encoded_value {
        match type_info.as_ref() {
            Some(TypeInfo::Integer { .. })
            | Some(TypeInfo::Logical { .. })
            | Some(TypeInfo::Enumeration(_)) => {
                const_value = Some(value.parse::<i128>().map_err(|_| {
                    format!(
                        "{}: corrupt .amod file (invalid integer parameter value for '{}'); rebuild the provider module",
                        path.display(),
                        name
                    )
                })?);
            }
            Some(TypeInfo::Real { .. }) | Some(TypeInfo::DoublePrecision) => {
                const_real_value = Some(value.parse::<f64>().map_err(|_| {
                    format!(
                        "{}: corrupt .amod file (invalid real parameter value for '{}'); rebuild the provider module",
                        path.display(),
                        name
                    )
                })?);
            }
            _ => {
                return Err(format!(
                    "{}: corrupt .amod file (unexpected parameter value for '{}'); rebuild the provider module",
                    path.display(),
                    name
                ));
            }
        }
    }
    let mut attrs = HashSet::new();
    for attr in attr_str.split(", ").filter(|attr| !attr.is_empty()) {
        let known_attribute = matches!(
            attr,
            "allocatable"
                | "save"
                | "pointer"
                | "procptr"
                | "target"
                | "volatile"
                | "protected"
                | "threadprivate"
                | "private"
        ) || (version >= 19 && matches!(attr, "asynchronous" | "contiguous"));
        if version >= 17 && !known_attribute {
            return Err(format!(
                "{}: corrupt .amod file (unknown variable attribute '{}' for '{}'); rebuild the provider module",
                path.display(),
                attr,
                name
            ));
        }
        if version >= 17 && !attrs.insert(attr) {
            return Err(format!(
                "{}: corrupt .amod file (duplicate variable attribute '{}' for '{}'); rebuild the provider module",
                path.display(),
                attr,
                name
            ));
        }
        attrs.insert(attr);
    }
    let allocatable = attrs.contains("allocatable");
    let save = attrs.contains("save");
    let pointer = attrs.contains("pointer");
    let proc_pointer = attrs.contains("procptr");
    let target = attrs.contains("target");
    let asynchronous = attrs.contains("asynchronous");
    let contiguous = attrs.contains("contiguous");
    let volatile = attrs.contains("volatile");
    let protected = attrs.contains("protected");
    let openmp_threadprivate = attrs.contains("threadprivate");
    let access = if attrs.contains("private") {
        Access::Private
    } else {
        Access::Public
    };

    let mut ir_symbol = None;
    let mut deferred_char = false;
    let mut rank = 0usize;
    let mut dims = Vec::new();

    if let Some(meta) = meta_part {
        let parts: Vec<&str> = meta.split_whitespace().collect();
        let mut i = 0;
        while i < parts.len() {
            if parts[i] == "@ir" {
                if let Some(symbol) = parts.get(i + 1) {
                    ir_symbol = Some((*symbol).to_string());
                }
                i += 2;
            } else if parts[i] == "@charhex" {
                if let Some(hex) = parts.get(i + 1) {
                    const_char_value = Some(
                        hex_decode_bytes(hex)
                            .map(|bytes| crate::source_bytes::to_source_view(&bytes))
                            .ok_or_else(|| {
                                format!(
                                    "{}: corrupt .amod file (malformed @charhex value for '{}'); rebuild the provider module",
                                    path.display(),
                                    name
                                )
                            })?,
                    );
                } else if version >= 17 {
                    return Err(format!(
                        "{}: corrupt .amod file (missing @charhex value for '{}'); rebuild the provider module",
                        path.display(),
                        name
                    ));
                }
                i += 2;
            } else if parts[i] == "@deferred_char" {
                deferred_char = true;
                i += 1;
            } else if parts[i] == "@rank" {
                if let Some(value) = parts.get(i + 1) {
                    // The writer always emits an integer here (declared_rank).
                    // A parse failure means a corrupt or version-mismatched
                    // .amod; defaulting to 0 would silently demote an array to
                    // a scalar and miscompile every use (audit T3).
                    rank = value.parse().map_err(|_| {
                        format!(
                            "{}: corrupt .amod file (malformed @rank value for '{}'); rebuild the provider module",
                            path.display(),
                            name
                        )
                    })?;
                } else {
                    return Err(format!(
                        "{}: corrupt .amod file (missing @rank value for '{}'); rebuild the provider module",
                        path.display(),
                        name
                    ));
                }
                i += 2;
            } else if parts[i] == "@dims" {
                // Parse dimension pairs: @dims 1:5 1:10 ...  The writer emits
                // i64:i64 pairs; a non-integer bound is corruption, and
                // defaulting to 1 would silently give the array wrong shape.
                i += 1;
                while i < parts.len() && parts[i].contains(':') && !parts[i].starts_with('@') {
                    let pair = parts[i];
                    if let Some((lo_s, ext_s)) = pair.split_once(':') {
                        let bound = |s: &str, what: &str| -> Result<i64, String> {
                            s.parse::<i64>().map_err(|_| {
                                format!(
                                    "{}: corrupt .amod file (malformed @dims {what} for '{}'); rebuild the provider module",
                                    path.display(),
                                    name
                                )
                            })
                        };
                        dims.push((bound(lo_s, "lower bound")?, bound(ext_s, "extent")?));
                    } else {
                        return Err(format!(
                            "{}: corrupt .amod file (malformed @dims entry for '{}'); rebuild the provider module",
                            path.display(),
                            name
                        ));
                    }
                    i += 1;
                }
            } else {
                if version >= 17 {
                    return Err(format!(
                        "{}: corrupt .amod file (unknown variable metadata '{}' for '{}'); rebuild the provider module",
                        path.display(),
                        parts[i],
                        name
                    ));
                }
                i += 1;
            }
        }
    }

    Ok(AmodVar {
        name,
        type_info,
        is_parameter: is_param,
        allocatable,
        save,
        pointer,
        proc_pointer,
        target,
        asynchronous,
        contiguous,
        volatile,
        protected,
        openmp_threadprivate,
        ir_symbol,
        deferred_char,
        rank: rank.max(dims.len()),
        dims,
        const_value,
        const_real_value,
        const_char_value,
        access,
    })
}

/// Split a comma-separated attribute list while honoring paren depth and
/// double-quoted strings. The naive `split(", ")` mangled values like
/// `result_array_bounds="(max(n, 0))"` because the inner `, ` between
/// `n` and `0` matched the separator and split the value across two
/// chunks — losing the bounds and forcing the resolver to fall back to
/// AssumedShape, which broke runtime-shape result allocation for
/// abbreviated SMP bodies pulling specs out of .amod.
fn split_attrs_top_level(attrs: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut in_quote = false;
    let mut start = 0usize;
    let bytes = attrs.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        match ch {
            '"' if !in_quote => in_quote = true,
            '"' if in_quote => in_quote = false,
            '(' | '[' if !in_quote => depth += 1,
            ')' | ']' if !in_quote => depth -= 1,
            ',' if !in_quote && depth == 0 => {
                let chunk = attrs[start..i].trim();
                if !chunk.is_empty() {
                    out.push(chunk.to_string());
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail = attrs[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

fn parse_module_nature_token(token: &str) -> Option<UseNature> {
    match token.trim() {
        "normal" => Some(UseNature::Normal),
        "intrinsic" => Some(UseNature::Intrinsic),
        "non_intrinsic" => Some(UseNature::NonIntrinsic),
        _ => None,
    }
}

fn parse_proc(
    header: &str,
    lines: &mut std::iter::Peekable<std::str::Lines>,
    version: u32,
    path: &Path,
) -> Result<AmodProc, String> {
    let is_func = header.starts_with("@function ");
    let rest = if is_func {
        header.strip_prefix("@function ").unwrap()
    } else {
        header.strip_prefix("@subroutine ").unwrap()
    };

    // Parse: name [-> return_type][, pure][, elemental]
    let (name_and_ret, attrs_str) = {
        // Use depth-aware split so attribute values containing commas
        // inside parens (e.g. `result_array_bounds="(max(n, 0))"`)
        // don't split prematurely on the inner `, `.
        let mut depth: i32 = 0;
        let mut in_quote = false;
        let mut split_at: Option<usize> = None;
        let bytes = rest.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let ch = bytes[i] as char;
            match ch {
                '"' if !in_quote => in_quote = true,
                '"' if in_quote => in_quote = false,
                '(' | '[' if !in_quote => depth += 1,
                ')' | ']' if !in_quote => depth -= 1,
                ',' if !in_quote && depth == 0 => {
                    split_at = Some(i);
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        match split_at {
            Some(idx) => (rest[..idx].trim_end(), rest[idx + 1..].trim_start()),
            None => (rest.trim(), ""),
        }
    };

    let (name, return_type) = if let Some(arrow_idx) = name_and_ret.find(" -> ") {
        let n = &name_and_ret[..arrow_idx];
        let rt = parse_type_info_for_version(
            name_and_ret[arrow_idx + 4..].trim(),
            version,
            path,
            "function result",
        )?;
        (n.trim().to_string(), rt)
    } else {
        (name_and_ret.trim().to_string(), None)
    };

    let attr_chunks = split_attrs_top_level(attrs_str);
    let pure = attr_chunks.iter().any(|a| a == "pure");
    let elemental = attr_chunks.iter().any(|a| a == "elemental");
    let external = attr_chunks.iter().any(|a| a == "external");
    let abstract_interface = attr_chunks.iter().any(|a| a == "abstract_interface");
    let is_separate_module_interface = attr_chunks.iter().any(|a| a == "module_interface");
    let is_separate_module_procedure = attr_chunks.iter().any(|a| a == "module_procedure");
    let result_allocatable = attr_chunks.iter().any(|a| a == "result_allocatable");
    let result_pointer = attr_chunks.iter().any(|a| a == "result_pointer");
    let result_procedure_iface = attr_chunks
        .iter()
        .find_map(|attr| attr.strip_prefix("result_procedure="))
        .map(str::to_string);
    let result_procedure_iface_import = result_procedure_iface.as_ref().and_then(|local| {
        let original = attr_chunks
            .iter()
            .find_map(|attr| attr.strip_prefix("result_procedure_original="))?;
        let source_module = attr_chunks
            .iter()
            .find_map(|attr| attr.strip_prefix("result_procedure_module="))?;
        let source_nature = attr_chunks
            .iter()
            .find_map(|attr| attr.strip_prefix("result_procedure_nature="))
            .and_then(parse_module_nature_token)?;
        Some(UseRename {
            local: local.clone(),
            original: original.to_string(),
            source_module: source_module.to_string(),
            source_nature,
        })
    });
    let result_rank_values: Vec<_> = attr_chunks
        .iter()
        .filter_map(|attr| attr.strip_prefix("result_rank="))
        .collect();
    let result_rank = match result_rank_values.as_slice() {
        [] => 0,
        [value] => value.parse::<u8>().map_err(|_| {
            format!(
                "{}: corrupt .amod file (invalid function result rank '{}'); rebuild the provider module",
                path.display(),
                value
            )
        })?,
        _ if version >= 17 => {
            return Err(format!(
                "{}: corrupt .amod file (duplicate function result rank); rebuild the provider module",
                path.display()
            ));
        }
        _ => 0,
    };
    // Sprint35-SMP Phase 2: optional `result_name=NAME` when the
    // source used a `result(NAME)` clause that differs from the
    // function name. Otherwise the result variable shares the name.
    let result_name = attr_chunks
        .iter()
        .find_map(|attr| attr.strip_prefix("result_name="))
        .map(|s| s.trim().to_string());
    let access = if attr_chunks.iter().any(|attr| attr == "private") {
        Access::Private
    } else {
        Access::Public
    };
    let binding_label = attr_chunks
        .iter()
        .find_map(|attr| attr.strip_prefix("bind=").map(|label| label.to_string()));
    let bind_c = binding_label.is_some() || attr_chunks.iter().any(|attr| attr == "bind_c");

    let kind = if is_func {
        SymbolKind::Function
    } else {
        SymbolKind::Subroutine
    };

    let mut args = Vec::new();

    // Parse body lines until @end.
    let mut saw_end = false;
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.starts_with("@end ") {
            saw_end = true;
            break;
        }
        if trimmed.starts_with("@arg ") {
            args.push(parse_arg(trimmed, version, path)?);
        }
        // Skip @abi and legacy @hint lines; explicit @arg records carry the
        // ABI-relevant metadata used by the reader.
    }

    let result_array_bounds = attr_chunks
        .iter()
        .find_map(|attr| attr.strip_prefix("result_array_bounds="))
        .map(|s| s.trim_matches('"').to_string());

    if version >= 17 && !saw_end {
        return Err(format!(
            "{}: corrupt .amod file (unterminated procedure '{}'); rebuild the provider module",
            path.display(),
            name
        ));
    }

    Ok(AmodProc {
        name,
        kind,
        return_type,
        result_allocatable,
        result_pointer,
        result_procedure_iface,
        result_procedure_iface_import,
        result_rank,
        result_name,
        result_array_bounds,
        pure,
        elemental,
        external,
        abstract_interface,
        is_separate_module_interface,
        is_separate_module_procedure,
        access,
        bind_c,
        binding_label,
        args,
    })
}

fn parse_arg(line: &str, version: u32, path: &Path) -> Result<AmodArg, String> {
    // @arg name : type[, intent(in/out/inout)][, optional][, value][, ...]
    let rest = line.strip_prefix("@arg ").unwrap_or(line);

    let (name, type_and_attrs) = if let Some(idx) = rest.find(" : ") {
        (&rest[..idx], &rest[idx + 3..])
    } else {
        (rest.trim(), "unknown")
    };

    let name = name.trim().to_string();
    let hidden = name.contains('@'); // e.g., label@len

    let (type_str, attr_str) = if let Some(idx) = type_and_attrs.find(", ") {
        (&type_and_attrs[..idx], &type_and_attrs[idx + 2..])
    } else {
        (type_and_attrs, "")
    };

    let type_info = parse_type_info_for_version(type_str.trim(), version, path, "procedure dummy")?;
    let intent = if attr_str.contains("intent(in)") && !attr_str.contains("intent(inout)") {
        Some(Intent::In)
    } else if attr_str.contains("intent(out)") {
        Some(Intent::Out)
    } else if attr_str.contains("intent(inout)") {
        Some(Intent::InOut)
    } else {
        None
    };

    let attr_chunks = split_attrs_top_level(attr_str);
    let has_attr = |expected: &str| {
        attr_chunks
            .iter()
            .any(|attr| attr.eq_ignore_ascii_case(expected))
    };
    let optional = has_attr("optional");
    let value = has_attr("value");
    let descriptor = has_attr("descriptor");
    let allocatable = has_attr("allocatable");
    let pointer = has_attr("pointer");
    let target = has_attr("target");
    let asynchronous = has_attr("asynchronous");
    let contiguous = has_attr("contiguous");
    let volatile = has_attr("volatile");
    let external = has_attr("external");
    let assumed_rank = has_attr("assumed-rank");
    // Sprint35-SMP Phase 1: parse `rank=N` if present. Emitted only when
    // the dummy is array-shaped; absence means rank 0 (scalar).
    let rank_values: Vec<_> = attr_chunks
        .iter()
        .filter_map(|attr| attr.strip_prefix("rank="))
        .collect();
    let rank = match rank_values.as_slice() {
        [] => 0,
        [value] => value.trim().parse::<u8>().map_err(|_| {
            format!(
                "{}: corrupt .amod file (invalid procedure dummy rank '{}'); rebuild the provider module",
                path.display(),
                value
            )
        })?,
        _ if version >= 17 => {
            return Err(format!(
                "{}: corrupt .amod file (duplicate procedure dummy rank); rebuild the provider module",
                path.display()
            ));
        }
        _ => 0,
    };
    let procedure_iface = attr_chunks.iter().find_map(|attr| {
        let inner = attr.strip_prefix("procedure(")?;
        inner.strip_suffix(')').map(|s| s.trim().to_string())
    });
    let procedure_iface_import = procedure_iface.as_ref().and_then(|local| {
        let original = attr_chunks
            .iter()
            .find_map(|attr| attr.strip_prefix("procedure_original="))?;
        let source_module = attr_chunks
            .iter()
            .find_map(|attr| attr.strip_prefix("procedure_module="))?;
        let source_nature = attr_chunks
            .iter()
            .find_map(|attr| attr.strip_prefix("procedure_nature="))
            .and_then(parse_module_nature_token)?;
        Some(UseRename {
            local: local.clone(),
            original: original.to_string(),
            source_module: source_module.to_string(),
            source_nature,
        })
    });
    let encoded_array_specs: Vec<_> = attr_chunks
        .iter()
        .filter_map(|attr| attr.strip_prefix("array_spec="))
        .collect();
    let array_spec = match encoded_array_specs.as_slice() {
        [] => None,
        [spec] => {
            let spec = spec.strip_prefix('"').and_then(|s| s.strip_suffix('"'));
            match spec.and_then(parse_dummy_array_spec) {
                Some(parsed) => Some(parsed),
                None if version >= 17 => {
                    return Err(format!(
                        "{}: corrupt .amod file (invalid procedure dummy array specification); rebuild the provider module",
                        path.display()
                    ));
                }
                None => None,
            }
        }
        _ if version >= 17 => {
            return Err(format!(
                "{}: corrupt .amod file (duplicate procedure dummy array specification); rebuild the provider module",
                path.display()
            ));
        }
        _ => None,
    };

    Ok(AmodArg {
        name,
        type_info,
        intent,
        optional,
        value,
        descriptor,
        allocatable,
        pointer,
        target,
        asynchronous,
        contiguous,
        volatile,
        hidden,
        external,
        procedure_iface,
        procedure_iface_import,
        assumed_rank,
        rank,
        array_spec,
    })
}

fn parse_type(
    header: &str,
    lines: &mut std::iter::Peekable<std::str::Lines>,
    version: u32,
    path: &Path,
) -> Result<crate::sema::type_layout::TypeLayout, String> {
    use crate::sema::type_layout::*;

    fn parse_field_default_init_token(token: &str) -> Option<FieldDefaultInit> {
        let payload = token.strip_prefix("@init=")?;
        if let Some(value) = payload.strip_prefix("int:") {
            return value.parse::<i128>().ok().map(FieldDefaultInit::Integer);
        }
        if let Some(value) = payload.strip_prefix("logical:") {
            return match value {
                "true" => Some(FieldDefaultInit::Logical(true)),
                "false" => Some(FieldDefaultInit::Logical(false)),
                _ => None,
            };
        }
        if let Some(value) = payload.strip_prefix("realbits:") {
            return u64::from_str_radix(value, 16)
                .ok()
                .map(f64::from_bits)
                .map(FieldDefaultInit::Real);
        }
        if let Some(value) = payload.strip_prefix("charhex:") {
            let decoded = crate::source_bytes::to_source_view(&hex_decode_bytes(value)?);
            return Some(FieldDefaultInit::Character(decoded));
        }
        if let Some(value) = payload.strip_prefix("exprhex:") {
            let decoded = String::from_utf8(hex_decode_bytes(value)?).ok()?;
            return decode_nested_field_default_init(&decoded);
        }
        if let Some(value) = payload.strip_prefix("procptr:") {
            return Some(FieldDefaultInit::ProcedurePointer(value.to_string()));
        }
        None
    }

    let name = header
        .strip_prefix("@type ")
        .unwrap_or("unknown")
        .trim()
        .to_string();
    let mut size = 0;
    let mut align = 1;
    let mut parent = None;
    let mut fields = Vec::new();
    let mut bound_procs = Vec::new();
    let mut final_procs = Vec::new();
    let mut owner_module = None;
    let mut type_tag = 0u64;
    let mut is_abstract = false;
    let mut saw_end = false;
    let mut saw_layout = false;
    let mut saw_tag = false;

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed == "@end type" {
            saw_end = true;
            break;
        }

        if let Some(rest) = trimmed.strip_prefix("@layout ") {
            if version >= 17 && saw_layout {
                return Err(format!(
                    "{}: corrupt .amod file (duplicate @layout for type '{}'); rebuild the provider module",
                    path.display(),
                    name
                ));
            }
            saw_layout = true;
            let mut parsed_size = None;
            let mut parsed_align = None;
            let mut saw_size = false;
            let mut saw_align = false;
            for part in rest.split_whitespace() {
                if let Some(v) = part.strip_prefix("size=") {
                    if version >= 17 && saw_size {
                        return Err(format!(
                            "{}: corrupt .amod file (duplicate @layout size for type '{}'); rebuild the provider module",
                            path.display(),
                            name
                        ));
                    }
                    saw_size = true;
                    parsed_size = v.parse::<usize>().ok();
                } else if let Some(v) = part.strip_prefix("align=") {
                    if version >= 17 && saw_align {
                        return Err(format!(
                            "{}: corrupt .amod file (duplicate @layout alignment for type '{}'); rebuild the provider module",
                            path.display(),
                            name
                        ));
                    }
                    saw_align = true;
                    parsed_align = v.parse::<usize>().ok();
                } else if version >= 17 {
                    return Err(format!(
                        "{}: corrupt .amod file (unknown @layout metadata '{}' for type '{}'); rebuild the provider module",
                        path.display(),
                        part,
                        name
                    ));
                }
            }
            if version >= 17 && (parsed_size.is_none() || parsed_align.is_none()) {
                return Err(format!(
                    "{}: corrupt .amod file (malformed @layout for type '{}'); rebuild the provider module",
                    path.display(),
                    name
                ));
            }
            size = parsed_size.unwrap_or(0);
            align = parsed_align.unwrap_or(1);
            if version >= 17 && (align == 0 || !align.is_power_of_two()) {
                return Err(format!(
                    "{}: corrupt .amod file (invalid @layout alignment '{}' for type '{}'); rebuild the provider module",
                    path.display(),
                    align,
                    name
                ));
            }
        } else if let Some(rest) = trimmed.strip_prefix("@extends ") {
            parent = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@field ") {
            // @field name : type @offset N @size M [@allocatable] [@pointer] [@target]
            if let Some((name_type, offset_part)) = rest.split_once(" @offset ") {
                let (fname, ftype_str) = match name_type.split_once(" : ") {
                    Some((fname, ftype))
                        if !fname.trim().is_empty() && !ftype.trim().is_empty() =>
                    {
                        (fname, ftype)
                    }
                    _ if version >= 17 => {
                        return Err(format!(
                            "{}: corrupt .amod file (malformed @field record in type '{}'); rebuild the provider module",
                            path.display(),
                            name
                        ));
                    }
                    _ => (name_type, "unknown"),
                };
                // Split off the size and any trailing attribute flags.
                let (offset_str, after_offset) = match offset_part.find(" @size ") {
                    Some(idx) => (&offset_part[..idx], &offset_part[idx + 7..]),
                    None if version >= 17 => {
                        return Err(format!(
                            "{}: corrupt .amod file (missing @field size for '{}'); rebuild the provider module",
                            path.display(),
                            fname.trim()
                        ));
                    }
                    None => (offset_part, "0"),
                };
                let mut size_str = after_offset;
                let mut dims: Vec<(i64, i64)> = Vec::new();
                let mut flag_tail: &str = "";
                if let Some(idx) = after_offset.find(" @dims ") {
                    size_str = &after_offset[..idx];
                    let dims_part = &after_offset[idx + 7..];
                    let (dims_str, tail) = if let Some(flag_idx) = dims_part.find(" @") {
                        (&dims_part[..flag_idx], &dims_part[flag_idx..])
                    } else {
                        (dims_part, "")
                    };
                    if version >= 17 && dims_str.trim().is_empty() {
                        return Err(format!(
                            "{}: corrupt .amod file (empty @field dimensions for '{}'); rebuild the provider module",
                            path.display(),
                            fname.trim()
                        ));
                    }
                    for dim in dims_str.split(',').map(str::trim) {
                        let parsed = dim.split_once(':').and_then(|(lower_str, upper_str)| {
                            if upper_str.contains(':') {
                                return None;
                            }
                            let lower = lower_str.trim().parse::<i64>().ok()?;
                            let upper = upper_str.trim().parse::<i64>().ok()?;
                            let extent = upper.checked_sub(lower)?.checked_add(1)?.max(0);
                            Some((lower, extent))
                        });
                        match parsed {
                            Some(dim) => dims.push(dim),
                            None if version >= 17 => {
                                return Err(format!(
                                    "{}: corrupt .amod file (malformed @field dimensions for '{}'); rebuild the provider module",
                                    path.display(),
                                    fname.trim()
                                ));
                            }
                            None => {}
                        }
                    }
                    flag_tail = tail;
                } else if let Some(idx) = after_offset.find(" @") {
                    size_str = &after_offset[..idx];
                    flag_tail = &after_offset[idx..];
                }
                let mut allocatable = false;
                let mut pointer = false;
                let mut deferred_char = false;
                let mut target = false;
                let mut declared_array = false;
                let mut procedure_pointer = false;
                let mut procedure_pointer_nopass = false;
                let mut default_init = None;
                let mut access = None;
                let mut field_owner_module = None;
                let mut flag_names = HashSet::new();
                let tokens: Vec<&str> = flag_tail.split_whitespace().collect();
                let mut token_index = 0;
                while token_index < tokens.len() {
                    match tokens[token_index] {
                        flag @ ("@allocatable" | "@pointer" | "@deferred_char" | "@procptr"
                        | "@nopass" | "@target" | "@declared_array") => {
                            if version >= 17 && !flag_names.insert(flag) {
                                return Err(format!(
                                    "{}: corrupt .amod file (duplicate @field metadata '{}' for '{}'); rebuild the provider module",
                                    path.display(),
                                    flag,
                                    fname.trim()
                                ));
                            }
                            match flag {
                                "@allocatable" => allocatable = true,
                                "@pointer" => pointer = true,
                                "@deferred_char" => deferred_char = true,
                                "@procptr" => procedure_pointer = true,
                                "@nopass" => procedure_pointer_nopass = true,
                                "@target" => target = true,
                                "@declared_array" => declared_array = true,
                                _ => unreachable!(),
                            }
                        }
                        "@access" => {
                            if access.is_some() {
                                return Err(format!(
                                    "{}: corrupt .amod file (duplicate @field accessibility for '{}'); rebuild the provider module",
                                    path.display(),
                                    fname.trim()
                                ));
                            }
                            let Some(value) = tokens.get(token_index + 1) else {
                                return Err(format!(
                                    "{}: corrupt .amod file (missing @field accessibility value for '{}'); rebuild the provider module",
                                    path.display(),
                                    fname.trim()
                                ));
                            };
                            access = Some(match *value {
                                "public" => Access::Public,
                                "private" => Access::Private,
                                _ => {
                                    return Err(format!(
                                        "{}: corrupt .amod file (invalid @field accessibility '{}' for '{}'); rebuild the provider module",
                                        path.display(),
                                        value,
                                        fname.trim()
                                    ));
                                }
                            });
                            token_index += 2;
                            continue;
                        }
                        "@owner" => {
                            if field_owner_module.is_some() {
                                return Err(format!(
                                    "{}: corrupt .amod file (duplicate @field owner for '{}'); rebuild the provider module",
                                    path.display(),
                                    fname.trim()
                                ));
                            }
                            let Some(value) = tokens.get(token_index + 1) else {
                                return Err(format!(
                                    "{}: corrupt .amod file (missing @field owner for '{}'); rebuild the provider module",
                                    path.display(),
                                    fname.trim()
                                ));
                            };
                            if value.starts_with('@') {
                                return Err(format!(
                                    "{}: corrupt .amod file (invalid @field owner '{}' for '{}'); rebuild the provider module",
                                    path.display(),
                                    value,
                                    fname.trim()
                                ));
                            }
                            field_owner_module = Some((*value).to_string());
                            token_index += 2;
                            continue;
                        }
                        token if token.starts_with("@init=") => {
                            if version >= 17 && default_init.is_some() {
                                return Err(format!(
                                    "{}: corrupt .amod file (duplicate @field initializer for '{}'); rebuild the provider module",
                                    path.display(),
                                    fname.trim()
                                ));
                            }
                            match parse_field_default_init_token(token) {
                                Some(init) => default_init = Some(init),
                                None if version >= 17 => {
                                    return Err(format!(
                                        "{}: corrupt .amod file (invalid @field initializer for '{}'); rebuild the provider module",
                                        path.display(),
                                        fname.trim()
                                    ));
                                }
                                None => {}
                            }
                        }
                        token if version >= 17 => {
                            return Err(format!(
                                "{}: corrupt .amod file (unknown @field metadata '{}' for '{}'); rebuild the provider module",
                                path.display(),
                                token,
                                fname.trim()
                            ));
                        }
                        _ => {}
                    }
                    token_index += 1;
                }
                let access = match access {
                    Some(access) => access,
                    None if version < AMOD_FIELD_ACCESS_VERSION => Access::Public,
                    None => {
                        return Err(format!(
                            "{}: corrupt .amod file (missing @field accessibility for '{}'); rebuild the provider module",
                            path.display(),
                            fname.trim()
                        ));
                    }
                };
                if version >= AMOD_FIELD_ACCESS_VERSION && field_owner_module.is_none() {
                    return Err(format!(
                        "{}: corrupt .amod file (missing @field owner for '{}'); rebuild the provider module",
                        path.display(),
                        fname.trim()
                    ));
                }
                declared_array |= !dims.is_empty();
                let ftype = parse_type_info_for_version(
                    ftype_str.trim(),
                    version,
                    path,
                    "derived-type field",
                )?;
                let offset = offset_str.trim().parse::<usize>().map_err(|_| {
                    format!(
                        "{}: corrupt .amod file (invalid @field offset for '{}'); rebuild the provider module",
                        path.display(),
                        fname.trim()
                    )
                })?;
                let field_size = size_str.trim().parse::<usize>().map_err(|_| {
                    format!(
                        "{}: corrupt .amod file (invalid @field size for '{}'); rebuild the provider module",
                        path.display(),
                        fname.trim()
                    )
                })?;
                fields.push(FieldLayout {
                    name: fname.trim().to_string(),
                    access,
                    owner_module: field_owner_module,
                    offset,
                    size: field_size,
                    dims,
                    declared_array,
                    type_info: ftype.unwrap_or(TypeInfo::Integer { kind: None }),
                    allocatable,
                    pointer,
                    deferred_char,
                    target,
                    procedure_pointer,
                    procedure_pointer_nopass,
                    default_init,
                });
            } else if version >= 17 {
                return Err(format!(
                    "{}: corrupt .amod file (malformed @field record in type '{}'); rebuild the provider module",
                    path.display(),
                    name
                ));
            }
        } else if let Some(rest) = trimmed.strip_prefix("@binds ") {
            if version >= 17 {
                let (binding, metadata) = rest.find(" @").map_or((rest.trim(), ""), |index| {
                    (rest[..index].trim(), rest[index + 1..].trim())
                });
                let (method, target) = if let Some((method, target)) = binding.split_once(" => ") {
                    (method.trim().to_string(), target.trim().to_string())
                } else {
                    let method = binding.trim().to_string();
                    (method.clone(), method)
                };
                if method.is_empty() || target.is_empty() {
                    return Err(format!(
                        "{}: corrupt .amod file (malformed @binds record); rebuild the provider module",
                        path.display()
                    ));
                }
                let tokens: Vec<_> = metadata.split_whitespace().collect();
                let mut abi_name = None;
                let mut nopass = false;
                let mut pass_name = None;
                let mut pass_index = None;
                let mut index = 0;
                while index < tokens.len() {
                    match tokens[index] {
                        "@nopass" if !nopass => {
                            nopass = true;
                            index += 1;
                        }
                        "@abi" if abi_name.is_none() => {
                            let value = tokens.get(index + 1).ok_or_else(|| {
                                format!(
                                    "{}: corrupt .amod file (missing @binds ABI name); rebuild the provider module",
                                    path.display()
                                )
                            })?;
                            abi_name = Some((*value).to_ascii_lowercase());
                            index += 2;
                        }
                        "@pass-name" if pass_name.is_none() => {
                            let value = tokens.get(index + 1).ok_or_else(|| {
                                format!(
                                    "{}: corrupt .amod file (missing @binds passed-object name); rebuild the provider module",
                                    path.display()
                                )
                            })?;
                            pass_name = Some((*value).to_string());
                            index += 2;
                        }
                        "@pass-index" if pass_index.is_none() => {
                            let value = tokens.get(index + 1).ok_or_else(|| {
                                format!(
                                    "{}: corrupt .amod file (missing @binds passed-object index); rebuild the provider module",
                                    path.display()
                                )
                            })?;
                            pass_index = Some(value.parse::<usize>().map_err(|_| {
                                format!(
                                    "{}: corrupt .amod file (invalid @binds passed-object index '{}'); rebuild the provider module",
                                    path.display(),
                                    value
                                )
                            })?);
                            index += 2;
                        }
                        token => {
                            return Err(format!(
                                "{}: corrupt .amod file (invalid or duplicate @binds metadata '{}'); rebuild the provider module",
                                path.display(),
                                token
                            ));
                        }
                    }
                }
                if nopass == (pass_name.is_some() || pass_index.is_some())
                    || (!nopass && (pass_name.is_none() || pass_index.is_none()))
                {
                    return Err(format!(
                        "{}: corrupt .amod file (@binds '{}' must encode exactly one of NOPASS or a passed-object name/index); rebuild the provider module",
                        path.display(),
                        method
                    ));
                }
                bound_procs.push(BoundProc {
                    method_name: method.clone(),
                    target_name: target,
                    abi_name: abi_name.unwrap_or_else(|| method.to_ascii_lowercase()),
                    nopass,
                    pass_name,
                    pass_index,
                });
            } else {
                let (clean, abi_name) =
                    if let Some((bind_part, abi_part)) = rest.split_once(" @abi ") {
                        (bind_part.trim().to_string(), abi_part.trim().to_lowercase())
                    } else {
                        (rest.trim().to_string(), String::new())
                    };
                let nopass = clean.contains(", nopass");
                let clean = clean.replace(", nopass", "");
                let (method, target) = if let Some((m, t)) = clean.split_once(" => ") {
                    (m.trim().to_string(), t.trim().to_string())
                } else {
                    let m = clean.trim().to_string();
                    (m.clone(), m)
                };
                let parsed_abi_name = if abi_name.is_empty() {
                    method.to_lowercase()
                } else {
                    abi_name
                };
                bound_procs.push(BoundProc {
                    method_name: method,
                    target_name: target,
                    abi_name: parsed_abi_name,
                    nopass,
                    pass_name: None,
                    pass_index: (!nopass).then_some(0),
                });
            }
        } else if let Some(rest) = trimmed.strip_prefix("@final ") {
            let mut parts = rest.split_whitespace();
            let name = parts.next().unwrap_or_default().to_string();
            let attributes: Vec<_> = parts.collect();
            let rank = match attributes
                .iter()
                .find_map(|part| part.strip_prefix("rank="))
            {
                Some(rank) => rank.parse().map_err(|_| {
                    format!(
                        "{}: corrupt .amod file (malformed @final rank '{}'); rebuild the provider module",
                        path.display(),
                        rank
                    )
                })?,
                None => usize::MAX,
            };
            let elemental = match attributes
                .iter()
                .find_map(|part| part.strip_prefix("elemental="))
            {
                Some("true") => true,
                Some("false") => false,
                Some(value) => {
                    return Err(format!(
                        "{}: corrupt .amod file (invalid @final elemental value '{}'); rebuild the provider module",
                        path.display(),
                        value
                    ));
                }
                None if version < AMOD_FINAL_ELEMENTAL_VERSION => false,
                None => {
                    return Err(format!(
                        "{}: corrupt .amod file (missing @final elemental metadata); rebuild the provider module",
                        path.display()
                    ));
                }
            };
            final_procs.push(crate::sema::type_layout::FinalProc {
                name,
                rank,
                elemental,
            });
        } else if let Some(rest) = trimmed.strip_prefix("@owner ") {
            owner_module = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@tag ") {
            if version >= 17 && saw_tag {
                return Err(format!(
                    "{}: corrupt .amod file (duplicate @tag for type '{}'); rebuild the provider module",
                    path.display(),
                    name
                ));
            }
            type_tag = rest.trim().parse().map_err(|_| {
                format!(
                    "{}: corrupt .amod file (invalid @tag for type '{}'); rebuild the provider module",
                    path.display(),
                    name
                )
            })?;
            saw_tag = true;
        } else if trimmed == "@abstract" {
            is_abstract = true;
        } else if version >= 17 {
            return Err(format!(
                "{}: corrupt .amod file (unknown derived-type directive '{}'); rebuild the provider module",
                path.display(),
                trimmed
            ));
        }
    }

    let owner_path = owner_module
        .as_ref()
        .map(|owner| owner.to_ascii_lowercase());
    for field in &mut fields {
        if field.owner_module.is_none() {
            field.owner_module.clone_from(&owner_module);
        }
    }
    if version >= 17 && !saw_end {
        return Err(format!(
            "{}: corrupt .amod file (unterminated derived type '{}'); rebuild the provider module",
            path.display(),
            name
        ));
    }
    if version >= 17 && !saw_layout {
        return Err(format!(
            "{}: corrupt .amod file (missing @layout for type '{}'); rebuild the provider module",
            path.display(),
            name
        ));
    }
    if version >= 17 && !saw_tag {
        return Err(format!(
            "{}: corrupt .amod file (missing @tag for type '{}'); rebuild the provider module",
            path.display(),
            name
        ));
    }
    if version >= 17 {
        for field in &fields {
            let field_end = field.offset.checked_add(field.size).ok_or_else(|| {
                format!(
                    "{}: corrupt .amod file (@field '{}' layout overflows); rebuild the provider module",
                    path.display(),
                    field.name
                )
            })?;
            if field_end > size {
                return Err(format!(
                    "{}: corrupt .amod file (@field '{}' exceeds type '{}' layout); rebuild the provider module",
                    path.display(),
                    field.name,
                    name
                ));
            }
        }
    }
    Ok(TypeLayout {
        name,
        owner_module,
        owner_scope: None,
        owner_path,
        size,
        align,
        fields,
        bound_procs,
        final_procs,
        type_tag,
        parent,
        is_abstract,
    })
}

fn parse_amod_type(
    header: &str,
    lines: &mut std::iter::Peekable<std::str::Lines>,
    version: u32,
    path: &Path,
) -> Result<AmodType, String> {
    let malformed = || {
        format!(
            "{}: corrupt .amod file (malformed @type accessibility); rebuild the provider module",
            path.display()
        )
    };
    let rest = header.strip_prefix("@type ").ok_or_else(malformed)?.trim();
    let (name, access) = match rest.rsplit_once(", ") {
        Some((name, "public")) if !name.trim().is_empty() => (name.trim(), Access::Public),
        Some((name, "private")) if !name.trim().is_empty() => (name.trim(), Access::Private),
        Some(_) => return Err(malformed()),
        None if version < AMOD_TYPE_ACCESS_VERSION && !rest.is_empty() => (rest, Access::Public),
        None => return Err(malformed()),
    };
    let layout_header = format!("@type {name}");
    Ok(AmodType {
        layout: parse_type(&layout_header, lines, version, path)?,
        access,
    })
}

fn parse_type_info(s: &str) -> Option<TypeInfo> {
    let s = s.trim();
    if s == "unknown" || s.is_empty() {
        return None;
    }
    if s == "double precision" {
        return Some(TypeInfo::DoublePrecision);
    }
    if s == "class(*)" {
        return Some(TypeInfo::ClassStar);
    }
    if s == "type(*)" {
        return Some(TypeInfo::TypeStar);
    }

    // integer[(K)]
    if s.starts_with("integer") {
        let kind = extract_kind(s);
        return Some(TypeInfo::Integer { kind });
    }
    if s.starts_with("real") {
        let kind = extract_kind(s);
        return Some(TypeInfo::Real { kind });
    }
    if s.starts_with("complex") {
        let kind = extract_kind(s);
        return Some(TypeInfo::Complex { kind });
    }
    if s.starts_with("logical") {
        let kind = extract_kind(s);
        return Some(TypeInfo::Logical { kind });
    }
    if s.starts_with("character") {
        let mut len = None;
        let mut kind = None;
        if let Some(inner) = s
            .strip_prefix("character(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            for spec in inner.split(',').map(str::trim) {
                if let Some(value) = spec.strip_prefix("len=") {
                    len = (value != ":").then(|| value.parse::<i64>().ok()).flatten();
                } else if let Some(value) = spec.strip_prefix("kind=") {
                    kind = value.parse::<u8>().ok();
                }
            }
        }
        return Some(TypeInfo::Character { len, kind });
    }
    if let Some(inner) = s.strip_prefix("type(").and_then(|r| r.strip_suffix(')')) {
        return Some(TypeInfo::Derived(inner.to_string()));
    }
    if let Some(inner) = s.strip_prefix("class(").and_then(|r| r.strip_suffix(')')) {
        return Some(TypeInfo::Class(inner.to_string()));
    }
    // l07 flag: preserves the type identity of enumeration-typed
    // symbols; re-registering the type itself (and its enumerators)
    // from a .amod is the l07 round-trip row.
    if let Some(inner) = s
        .strip_prefix("enumeration(")
        .and_then(|r| r.strip_suffix(')'))
    {
        return Some(TypeInfo::Enumeration(inner.to_string()));
    }

    None
}

fn parse_type_info_for_version(
    encoded: &str,
    version: u32,
    path: &Path,
    record: &str,
) -> Result<Option<TypeInfo>, String> {
    if version < 17 {
        return Ok(parse_type_info(encoded));
    }

    let corrupt = || {
        format!(
            "{}: corrupt .amod file (invalid {record} type '{}'); rebuild the provider module",
            path.display(),
            encoded
        )
    };
    let parse_kind = |prefix: &str| -> Result<u8, String> {
        let value = encoded
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(')'))
            .ok_or_else(corrupt)?;
        if value.is_empty() || value.bytes().any(|byte| !byte.is_ascii_digit()) {
            return Err(corrupt());
        }
        let kind = value.parse::<u8>().map_err(|_| corrupt())?;
        if kind == 0 {
            return Err(corrupt());
        }
        Ok(kind)
    };

    let parsed = if encoded.starts_with("integer") {
        Some(TypeInfo::Integer {
            kind: Some(parse_kind("integer(")?),
        })
    } else if encoded.starts_with("real") {
        Some(TypeInfo::Real {
            kind: Some(parse_kind("real(")?),
        })
    } else if encoded.starts_with("complex") {
        Some(TypeInfo::Complex {
            kind: Some(parse_kind("complex(")?),
        })
    } else if encoded.starts_with("logical") {
        Some(TypeInfo::Logical {
            kind: Some(parse_kind("logical(")?),
        })
    } else if encoded == "double precision" {
        Some(TypeInfo::DoublePrecision)
    } else if let Some(inner) = encoded
        .strip_prefix("character(len=")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let (len, kind) = inner.split_once(",kind=").ok_or_else(corrupt)?;
        let len = if len == ":" {
            None
        } else {
            Some(len.parse::<i64>().map_err(|_| corrupt())?)
        };
        if kind.is_empty() || kind.bytes().any(|byte| !byte.is_ascii_digit()) {
            return Err(corrupt());
        }
        let kind = kind.parse::<u8>().map_err(|_| corrupt())?;
        if kind == 0 {
            return Err(corrupt());
        }
        Some(TypeInfo::Character {
            len,
            kind: Some(kind),
        })
    } else if encoded == "class(*)" {
        Some(TypeInfo::ClassStar)
    } else if encoded == "type(*)" {
        Some(TypeInfo::TypeStar)
    } else if let Some(inner) = encoded
        .strip_prefix("type(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        (!inner.trim().is_empty()).then(|| TypeInfo::Derived(inner.to_string()))
    } else if let Some(inner) = encoded
        .strip_prefix("class(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        (!inner.trim().is_empty()).then(|| TypeInfo::Class(inner.to_string()))
    } else if let Some(inner) = encoded
        .strip_prefix("enumeration(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        (!inner.trim().is_empty()).then(|| TypeInfo::Enumeration(inner.to_string()))
    } else {
        None
    };

    parsed.map(Some).ok_or_else(corrupt)
}

fn extract_kind(s: &str) -> Option<u8> {
    if let Some(start) = s.find('(') {
        if let Some(end) = s.find(')') {
            return s[start + 1..end].parse().ok();
        }
    }
    None
}

/// Convert a loaded ModuleInterface's variables into ModuleGlobalInfo
/// entries for the lowering pass.
pub fn extract_module_globals(
    iface: &ModuleInterface,
) -> HashMap<(String, String), crate::ir::lower::ModuleGlobalInfo> {
    let mod_key = iface.module_name.to_lowercase();
    let mut out = HashMap::new();
    for var in &iface.variables {
        // Private vars/params get included so submodules can resolve
        // host-associated references through the same globals map.
        // The `private` flag lets the "filtered out by USE ONLY"
        // diagnostic skip them — ordinary USE would never see them.
        let inline_real_param = var.is_parameter
            && var.ir_symbol.is_none()
            && var.const_real_value.is_some()
            && matches!(
                var.type_info.as_ref(),
                Some(TypeInfo::Real { .. } | TypeInfo::DoublePrecision)
            );
        if var.is_parameter && var.ir_symbol.is_none() && !inline_real_param {
            continue;
        } // PARAMETERs with folded values inline; others still need storage
        if var.ir_symbol.is_some() || inline_real_param {
            let ir_sym = var.ir_symbol.clone().unwrap_or_default();
            let declared_rank = var.rank.max(var.dims.len());
            let derived_type = match var.type_info.as_ref() {
                Some(TypeInfo::Derived(name))
                    if !matches!(name.to_lowercase().as_str(), "c_ptr" | "c_funptr") =>
                {
                    Some(name.clone())
                }
                _ => None,
            };
            let ir_ty = if var.proc_pointer
                || (matches!(
                    var.type_info.as_ref(),
                    Some(TypeInfo::Character { len: None, .. })
                ) && (var.allocatable || var.pointer)
                    && declared_rank > 0)
            {
                crate::ir::types::IrType::Ptr(Box::new(crate::ir::types::IrType::Int(
                    crate::ir::types::IntWidth::I8,
                )))
            } else if var.pointer {
                match derived_type.as_deref() {
                    Some("c_ptr") | Some("c_funptr") => {
                        crate::ir::types::IrType::Int(crate::ir::types::IntWidth::I64)
                    }
                    Some(_) => crate::ir::types::IrType::Ptr(Box::new(
                        crate::ir::types::IrType::Int(crate::ir::types::IntWidth::I8),
                    )),
                    None => type_info_to_ir_type(var.type_info.as_ref()),
                }
            } else if let Some(type_name) = &derived_type {
                if let Some(layout) = iface
                    .types
                    .iter()
                    .map(|amod_type| &amod_type.layout)
                    .find(|layout| layout.name.eq_ignore_ascii_case(type_name))
                {
                    crate::ir::types::IrType::Array(
                        Box::new(crate::ir::types::IrType::Int(
                            crate::ir::types::IntWidth::I8,
                        )),
                        layout.size as u64,
                    )
                } else {
                    type_info_to_ir_type(var.type_info.as_ref())
                }
            } else if let Some(TypeInfo::Character { len: Some(n), .. }) = var.type_info.as_ref() {
                if *n <= 1 {
                    crate::ir::types::IrType::Int(crate::ir::types::IntWidth::I8)
                } else {
                    crate::ir::types::IrType::Array(
                        Box::new(crate::ir::types::IrType::Int(
                            crate::ir::types::IntWidth::I8,
                        )),
                        *n as u64,
                    )
                }
            } else {
                type_info_to_ir_type(var.type_info.as_ref())
            };
            out.insert(
                (mod_key.clone(), var.name.to_lowercase()),
                crate::ir::lower::ModuleGlobalInfo {
                    symbol: ir_sym,
                    ty: ir_ty,
                    dims: var.dims.clone(),
                    declared_rank,
                    // Pointer arrays use the same 392-byte descriptor
                    // representation as allocatable arrays. Preserve that
                    // representation across the .amod boundary just as
                    // collect_module_globals does in the defining unit.
                    allocatable: var.allocatable || (var.pointer && declared_rank > 0),
                    is_pointer: var.pointer,
                    volatile: var.volatile,
                    openmp_threadprivate: var.openmp_threadprivate,
                    deferred_char: var.deferred_char,
                    derived_type,
                    char_kind: match var.type_info.as_ref() {
                        Some(crate::sema::symtab::TypeInfo::Character { len: Some(n), .. }) => {
                            crate::ir::lower::CharKind::Fixed(*n)
                        }
                        _ if var.deferred_char && var.rank == 0 => {
                            crate::ir::lower::CharKind::Deferred
                        }
                        _ => crate::ir::lower::CharKind::None,
                    },
                    logical_kind: match var.type_info.as_ref() {
                        Some(TypeInfo::Logical { kind }) => {
                            Some(kind.unwrap_or_else(crate::driver::defaults::default_int_kind))
                        }
                        _ => None,
                    },
                    const_value: var.const_value,
                    const_real_value: var.const_real_value,
                    external: true,
                    private: var.access == Access::Private,
                },
            );
        }
    }
    out
}

fn procedure_abi_owner<'a>(iface: &'a ModuleInterface, proc: &AmodProc) -> &'a str {
    if proc.is_separate_module_interface || proc.is_separate_module_procedure {
        iface
            .submodule_ancestor
            .as_deref()
            .unwrap_or(&iface.module_name)
    } else {
        &iface.module_name
    }
}

fn procedure_abi_link_name(iface: &ModuleInterface, proc: &AmodProc) -> String {
    if let Some(binding_label) = proc
        .binding_label
        .as_ref()
        .filter(|label| !label.is_empty())
    {
        return binding_label.clone();
    }
    if proc.external && !proc.is_separate_module_interface && !proc.is_separate_module_procedure {
        return proc.name.to_ascii_lowercase();
    }
    module_procedure_link_name(procedure_abi_owner(iface, proc), &proc.name.to_lowercase())
}

/// Extract optional-parameter masks from a loaded ModuleInterface.
pub fn extract_optional_params(iface: &ModuleInterface) -> HashMap<String, Vec<bool>> {
    let mut out = HashMap::new();
    for proc in &iface.procedures {
        let visible_args: Vec<&AmodArg> = proc.args.iter().filter(|a| !a.hidden).collect();
        let flags: Vec<bool> = visible_args.iter().map(|a| a.optional).collect();
        let key = proc.name.to_lowercase();
        out.insert(procedure_abi_link_name(iface, proc), flags.clone());
        if flags.iter().any(|flag| *flag) {
            out.insert(key, flags);
        }
    }
    out
}

/// Extract char_len_star_params from a loaded ModuleInterface.
/// For each procedure with character(len=*) args, produces a
/// Vec<bool> (per-position, true = assumed-length character).
pub fn extract_char_len_star_params(iface: &ModuleInterface) -> HashMap<String, Vec<bool>> {
    let mut out = HashMap::new();
    for proc in &iface.procedures {
        let is_bind_c = proc.bind_c;
        let visible_args: Vec<&AmodArg> = proc.args.iter().filter(|a| !a.hidden).collect();
        let hidden_len_args: HashSet<String> = proc
            .args
            .iter()
            .filter(|a| a.hidden)
            .filter_map(|a| a.name.strip_suffix("@len"))
            .map(|name| name.to_lowercase())
            .collect();
        let flags: Vec<bool> = if hidden_len_args.is_empty() {
            visible_args
                .iter()
                .map(|a| {
                    matches!(a.type_info, Some(TypeInfo::Character { len: None, .. }))
                        && !a.allocatable
                        && !is_bind_c
                })
                .collect()
        } else {
            visible_args
                .iter()
                .map(|a| hidden_len_args.contains(&a.name.to_lowercase()))
                .collect()
        };
        if !flags.is_empty() {
            let key = proc.name.to_lowercase();
            out.insert(key.clone(), flags.clone());
            out.insert(procedure_abi_link_name(iface, proc), flags);
        }
    }
    out
}

/// Extract descriptor_params from a loaded ModuleInterface.
/// For each procedure with descriptor-backed dummies, produces a
/// Vec<bool> (per-position, true = pass the 392-byte descriptor).
pub fn extract_descriptor_params(iface: &ModuleInterface) -> HashMap<String, Vec<bool>> {
    let mut out = HashMap::new();
    for proc in &iface.procedures {
        let visible_args: Vec<&AmodArg> = proc.args.iter().filter(|a| !a.hidden).collect();
        let flags: Vec<bool> = visible_args.iter().map(|a| a.descriptor).collect();
        if !flags.is_empty() {
            let key = proc.name.to_lowercase();
            out.insert(key.clone(), flags.clone());
            out.insert(procedure_abi_link_name(iface, proc), flags);
        }
    }
    out
}

fn type_info_to_ir_type(info: Option<&TypeInfo>) -> crate::ir::types::IrType {
    use crate::ir::types::{FloatWidth, IntWidth, IrType};
    let concrete_kind = info.and_then(|type_info| {
        crate::sema::type_layout::concrete_intrinsic_kind(
            type_info,
            crate::driver::defaults::current(),
        )
    });
    match info {
        Some(TypeInfo::Derived(name)) => {
            let lower = name.to_lowercase();
            if let Some(kind) = crate::sema::resolve::type_resolution::ieee_opaque_int_kind(&lower)
            {
                IrType::int_from_kind(kind)
            } else if lower == "c_ptr" || lower == "c_funptr" {
                IrType::Int(IntWidth::I64)
            } else {
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))
            }
        }
        Some(TypeInfo::Integer { .. }) => IrType::Int(match concrete_kind {
            Some(1) => IntWidth::I8,
            Some(2) => IntWidth::I16,
            Some(8) => IntWidth::I64,
            Some(16) => IntWidth::I128,
            _ => IntWidth::I32,
        }),
        Some(TypeInfo::Real { .. }) => IrType::Float(match concrete_kind {
            Some(8) => FloatWidth::F64,
            _ => FloatWidth::F32,
        }),
        Some(TypeInfo::DoublePrecision) => IrType::Float(FloatWidth::F64),
        Some(TypeInfo::Complex { .. }) => {
            let fw = match concrete_kind {
                Some(8) => FloatWidth::F64,
                _ => FloatWidth::F32,
            };
            IrType::Array(Box::new(IrType::Float(fw)), 2)
        }
        Some(TypeInfo::Logical { .. }) => {
            match concrete_kind.expect("LOGICAL has an intrinsic kind") {
                1 => IrType::Int(IntWidth::I8),
                2 => IrType::Int(IntWidth::I16),
                8 => IrType::Int(IntWidth::I64),
                16 => IrType::Int(IntWidth::I128),
                _ => IrType::Bool,
            }
        }
        Some(TypeInfo::Character { .. }) => IrType::Int(IntWidth::I8),
        _ => IrType::Int(IntWidth::I32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Audit T3: a corrupt @rank/@dims in a .amod (the writer only ever emits
    // integers) must fail loudly, not silently demote the array's shape.
    #[test]
    fn parse_var_rejects_corrupt_rank() {
        let error = parse_var(
            "@var a : integer(4) @rank notanumber",
            false,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap_err();
        assert!(error.contains("malformed @rank"), "{error}");
    }

    #[test]
    fn parse_var_rejects_corrupt_dims() {
        let error = parse_var(
            "@var a : integer(4) @dims 1:oops",
            false,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap_err();
        assert!(error.contains("malformed @dims"), "{error}");
    }

    #[test]
    fn module_variable_memory_attributes_require_current_schema() {
        let var = parse_var(
            "@var values : integer(4), target, asynchronous, contiguous",
            false,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert!(var.asynchronous);
        assert!(var.contiguous);

        let error = parse_var(
            "@var values : integer(4), asynchronous",
            false,
            AMOD_VERSION - 1,
            Path::new("stale.amod"),
        )
        .expect_err("older schema accepted a new variable attribute");
        assert!(error.contains("unknown variable attribute"), "{error}");
    }

    #[test]
    fn parse_var_preserves_full_width_integer_parameter() {
        let var = parse_var(
            "@var big : integer(16) = 170141183460469231731687303715884105727",
            true,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert_eq!(var.const_value, Some(i128::MAX));
    }

    #[test]
    fn parse_var_rejects_invalid_integer_parameter() {
        let error = parse_var(
            "@var big : integer(16) = 170141183460469231731687303715884105728",
            true,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap_err();
        assert!(error.contains("invalid integer parameter value"), "{error}");
    }

    #[test]
    fn character_kind_type_info_round_trips() {
        for info in [
            TypeInfo::Character {
                len: Some(8),
                kind: Some(4),
            },
            TypeInfo::Character {
                len: None,
                kind: Some(4),
            },
        ] {
            let encoded = type_info_to_string(Some(&info), DefaultKinds::new(4, 4));
            assert_eq!(parse_type_info(&encoded), Some(info));
            assert!(encoded.contains("kind=4"), "{encoded}");
        }
    }

    #[test]
    fn legacy_character_type_info_keeps_unknown_kind() {
        assert_eq!(
            parse_type_info("character(len=8)"),
            Some(TypeInfo::Character {
                len: Some(8),
                kind: None,
            })
        );
        assert_eq!(
            parse_type_info("character(len=:)"),
            Some(TypeInfo::Character {
                len: None,
                kind: None,
            })
        );
    }

    #[test]
    fn character_kind_survives_all_type_record_parsers() {
        let var = parse_var(
            "@var text : character(len=8,kind=4)",
            false,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert_eq!(
            var.type_info,
            Some(TypeInfo::Character {
                len: Some(8),
                kind: Some(4),
            })
        );

        let arg = parse_arg(
            "  @arg text : character(len=:,kind=4), intent(in)",
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert_eq!(
            arg.type_info,
            Some(TypeInfo::Character {
                len: None,
                kind: Some(4),
            })
        );

        let mut lines = "@end function\n".lines().peekable();
        let proc = parse_proc(
            "@function make_text -> character(len=8,kind=4)",
            &mut lines,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert_eq!(
            proc.return_type,
            Some(TypeInfo::Character {
                len: Some(8),
                kind: Some(4),
            })
        );
    }

    #[test]
    fn current_schema_rejects_ambiguous_intrinsic_types_in_every_record_family() {
        let bodies = [
            "@var value : integer\n".to_string(),
            "@param value : real = 1.0\n".to_string(),
            "@function value -> complex\n@end function\n".to_string(),
            "@subroutine consume\n  @arg value : logical\n@end subroutine\n".to_string(),
            "@type item_t, public\n  @layout size=4 align=4\n  @field value : integer @offset 0 @size 4 @access public @owner schema_test\n  @owner schema_test\n  @tag 1\n@end type\n".to_string(),
        ];
        for body in bodies {
            let text = add_integrity_headers(format!(
                "#!amod {AMOD_VERSION}\n# module: schema_test\n# default-access: public\n\n{body}"
            ));
            let error = read_amod_content(&text, Path::new("schema_test.amod"))
                .expect_err("ambiguous current-version intrinsic type was accepted");
            assert!(
                error.contains("corrupt .amod file")
                    && error.contains("invalid")
                    && error.contains("type")
                    && error.contains("rebuild the provider module"),
                "{body:?}: {error}"
            );
        }
    }

    #[test]
    fn current_schema_rejects_malformed_intrinsic_selectors() {
        for encoded in [
            "integer(foo)",
            "real()",
            "complex(garbage)",
            "logical(kind=bad)",
            "character(len=4)",
        ] {
            let error = parse_type_info_for_version(
                encoded,
                AMOD_VERSION,
                Path::new("schema_test.amod"),
                "variable",
            )
            .expect_err("malformed current-version type was accepted");
            assert!(
                error.contains("invalid variable type"),
                "{encoded}: {error}"
            );
        }
    }

    #[test]
    fn current_schema_rejects_malformed_enum_and_interface_records() {
        let bodies = [
            "@enumtype friend color red green\n",
            "@enumtype public color\n",
            "@enumtype public color red RED\n",
            "@interface pick, public\n  @specific pick_i\n@end interface\n",
            "@interface pick\n@end interface\n",
            "@interface pick\n  @specific pick_i\n  @specific PICK_I\n@end interface\n",
            "@interface pick\n  @specific pick_i\n",
            "@interface pick\n  @unknown pick_i\n@end interface\n",
        ];
        for body in bodies {
            let text = add_integrity_headers(format!(
                "#!amod {AMOD_VERSION}\n# module: schema_test\n# default-access: public\n\n{body}"
            ));
            let error = read_amod_content(&text, Path::new("schema_test.amod"))
                .expect_err("malformed current-version enum/interface record was accepted");
            assert!(
                error.contains("corrupt .amod file")
                    && error.contains("rebuild the provider module"),
                "{body:?}: {error}"
            );
        }
    }

    #[test]
    fn current_schema_rejects_malformed_abi_layout_metadata() {
        let valid_type = "@type item_t, public\n  @layout size=8 align=4\n  @field value : integer(4) @offset 4 @size 4 @dims 1:1 @access public @owner schema_test\n  @owner schema_test\n  @tag 1\n@end type\n";
        let malformed = [
            valid_type.replace("size=8", "size=oops"),
            valid_type.replace("align=4", "align=0"),
            valid_type.replace("@offset 4", "@offset oops"),
            valid_type.replace("@size 4", "@size oops"),
            valid_type.replace("@dims 1:1", "@dims 1:oops"),
            valid_type.replace("@access public", "@unknown"),
            valid_type.replace("@access public", "@init=int:oops @access public"),
            valid_type.replace("@tag 1", "@tag oops"),
            valid_type.replace("@offset 4", "@offset 8"),
        ];
        for body in malformed {
            let text = add_integrity_headers(format!(
                "#!amod {AMOD_VERSION}\n# module: schema_test\n# default-access: public\n\n{body}"
            ));
            let error = read_amod_content(&text, Path::new("schema_test.amod"))
                .expect_err("malformed current-version ABI layout metadata was accepted");
            assert!(
                error.contains("corrupt .amod file")
                    && error.contains("rebuild the provider module"),
                "{body:?}: {error}"
            );
        }
    }

    #[test]
    fn current_schema_rejects_malformed_procedure_shape_metadata() {
        let bodies = [
            "@function make -> integer(4), result_rank=oops\n@end function\n",
            "@subroutine take\n  @arg value : integer(4), rank=oops\n@end subroutine\n",
            "@subroutine take\n  @arg value : integer(4), rank=1, array_spec=\"broken\"\n@end subroutine\n",
        ];
        for body in bodies {
            let text = add_integrity_headers(format!(
                "#!amod {AMOD_VERSION}\n# module: schema_test\n# default-access: public\n\n{body}"
            ));
            let error = read_amod_content(&text, Path::new("schema_test.amod"))
                .expect_err("malformed current-version procedure shape metadata was accepted");
            assert!(
                error.contains("corrupt .amod file")
                    && error.contains("rebuild the provider module"),
                "{body:?}: {error}"
            );
        }
    }

    #[test]
    fn current_schema_cross_checks_pass_name_and_index() {
        let source = format!(
            "#!amod {AMOD_VERSION}\n# module: pass_schema\n# default-access: public\n\n@subroutine apply\n  @arg delta : integer(4), intent(in)\n  @arg self : class(item_t), intent(inout)\n@end subroutine\n\n@type item_t, public\n  @layout size=4 align=4\n  @field value : integer(4) @offset 0 @size 4 @access public @owner pass_schema\n  @binds run => apply @pass-name self @pass-index 0 @abi apply\n  @owner pass_schema\n  @tag 1\n@end type\n"
        );
        let text = add_integrity_headers(source.clone());
        let error = read_amod_content(&text, Path::new("pass_schema.amod"))
            .expect_err("disagreeing PASS name/index was accepted");
        assert!(
            error.contains("passed-object name/index disagree"),
            "{error}"
        );

        let missing = add_integrity_headers(source.replace(" @pass-name self @pass-index 0", ""));
        let error = read_amod_content(&missing, Path::new("pass_schema.amod"))
            .expect_err("binding without a PASS form was accepted");
        assert!(error.contains("must encode exactly one"), "{error}");
    }

    #[test]
    fn current_schema_validates_passed_object_characteristics() {
        let source = format!(
            "#!amod {AMOD_VERSION}\n# module: pass_schema\n# default-access: public\n\n@subroutine apply\n  @arg self : class(item_t), intent(inout)\n@end subroutine\n\n@type item_t, public\n  @layout size=4 align=4\n  @field value : integer(4) @offset 0 @size 4 @access public @owner pass_schema\n  @binds run => apply @pass-name self @pass-index 0 @abi apply\n  @owner pass_schema\n  @tag 1\n@end type\n"
        );
        let malformed = [
            ("class(item_t)", "integer(4)"),
            ("class(item_t)", "class(other_t)"),
            (
                "class(item_t), intent(inout)",
                "class(item_t), intent(inout), rank=1, array_spec=\"(:)\"",
            ),
            (
                "class(item_t), intent(inout)",
                "class(item_t), intent(inout), pointer",
            ),
            (
                "class(item_t), intent(inout)",
                "class(item_t), intent(inout), allocatable",
            ),
            (
                "class(item_t), intent(inout)",
                "class(item_t), intent(inout), value",
            ),
        ];
        for (from, to) in malformed {
            let text = add_integrity_headers(source.replacen(from, to, 1));
            let error = read_amod_content(&text, Path::new("pass_schema.amod"))
                .expect_err("malformed passed-object characteristics were accepted");
            assert!(
                error.contains("passed-object dummy")
                    && error.contains("corrupt .amod file")
                    && error.contains("rebuild the provider module"),
                "{error}"
            );
        }

        let optional = add_integrity_headers(source.replacen(
            "class(item_t), intent(inout)",
            "class(item_t), intent(inout), optional",
            1,
        ));
        read_amod_content(&optional, Path::new("pass_schema.amod"))
            .expect("OPTIONAL passed-object dummy is valid");
    }

    #[test]
    fn current_schema_validates_passed_object_against_defining_type() {
        let source = format!(
            "#!amod {AMOD_VERSION}\n# module: pass_schema\n# default-access: public\n\n@subroutine base_touch\n  @arg self : class(base_t), intent(inout)\n@end subroutine\n\n@subroutine child_touch\n  @arg self : class(override_t), intent(inout)\n@end subroutine\n\n@type inherited_t, public\n  @layout size=0 align=1\n  @extends pass_schema::base_t\n  @binds touch => base_touch @pass-name self @pass-index 0 @abi base_touch\n  @owner pass_schema\n  @tag 2\n@end type\n\n@type override_t, public\n  @layout size=0 align=1\n  @extends pass_schema::base_t\n  @binds touch => child_touch @pass-name self @pass-index 0 @abi child_touch\n  @owner pass_schema\n  @tag 3\n@end type\n\n@type base_t, public\n  @layout size=0 align=1\n  @binds touch => base_touch @pass-name self @pass-index 0 @abi base_touch\n  @owner pass_schema\n  @tag 1\n@end type\n"
        );
        let text = add_integrity_headers(source);
        read_amod_content(&text, Path::new("pass_schema.amod"))
            .expect("inherited and overridden bindings have truthful defining types");
    }

    #[test]
    fn current_schema_requires_local_binding_procedure_metadata() {
        let source = format!(
            "#!amod {AMOD_VERSION}\n# module: pass_schema\n# default-access: public\n\n@type item_t, public\n  @layout size=0 align=1\n  @binds run => apply @pass-name self @pass-index 0 @abi apply\n  @owner pass_schema\n  @tag 1\n@end type\n"
        );
        let text = add_integrity_headers(source);
        let error = read_amod_content(&text, Path::new("pass_schema.amod"))
            .expect_err("local binding without procedure metadata was accepted");
        assert!(error.contains("missing procedure metadata"), "{error}");
    }

    #[test]
    fn current_schema_allows_external_inherited_binding_metadata() {
        let source = format!(
            "#!amod {AMOD_VERSION}\n# module: child_schema\n# default-access: public\n\n@subroutine touch\n  @arg value : integer(4), intent(inout)\n@end subroutine\n\n@type child_t, public\n  @layout size=0 align=1\n  @extends ancestor_schema::base_t\n  @binds touch => afs_modproc$v2$m$ancestor_schema$p$touch @pass-name self @pass-index 0 @abi touch\n  @owner child_schema\n  @tag 2\n@end type\n\n@type base_t, public\n  @layout size=0 align=1\n  @binds touch => afs_modproc$v2$m$ancestor_schema$p$touch @pass-name self @pass-index 0 @abi touch\n  @owner ancestor_schema\n  @tag 1\n@end type\n"
        );
        let text = add_integrity_headers(source);
        read_amod_content(&text, Path::new("child_schema.amod"))
            .expect("external inherited binding may rely on its provider artifact");
    }

    #[test]
    fn current_schema_rejects_malformed_reserved_module_procedure_targets() {
        let source = format!(
            "#!amod {AMOD_VERSION}\n# module: malformed_target\n# default-access: public\n\n@type item_t, public\n  @layout size=0 align=1\n  @binds run => afs_modproc$v2$m$$p$decoy_proc @nopass @abi run\n  @owner malformed_target\n  @tag 1\n@end type\n"
        );
        let text = add_integrity_headers(source);
        let error = read_amod_content(&text, Path::new("malformed_target.amod"))
            .expect_err("malformed reserved module-procedure target was accepted");

        assert!(
            error.contains("malformed encoded module-procedure target")
                && error.contains("@binds")
                && error.contains("rebuild the provider module"),
            "{error}"
        );
    }

    #[test]
    fn round_trip_physics_amod() {
        let amod_text = r#"#!amod 2
# module: physics
# source: physics.f90
# checksum: sha256:abc123

@uses iso_c_binding

@var call_count : integer, save @ir afs_mod_physics_call_count

@param gravity : real = 9

@function kinetic_energy -> real, pure
  @abi cc=aapcs64 hidden_char_lens=0
  @arg self : class(particle), intent(in)
    @abi pass=x0 width=8
  @arg vx : real, intent(in)
    @abi pass=x1 width=8
  @hint leaf cost=27
@end function

@subroutine apply_force
  @abi cc=aapcs64 hidden_char_lens=0
  @arg p : type(particle), intent(inout)
    @abi pass=x0 width=8
  @arg dt : real, intent(in), optional
    @abi pass=x1 width=8
  @hint leaf cost=14
@end subroutine

@type particle
  @layout size=12 align=4
  @field x : real @offset 0 @size 4
  @field y : real @offset 4 @size 4
  @field mass : real @offset 8 @size 4
  @binds kinetic_energy
  @final afs_modproc_physics_finish_particles rank=1
  @tag 1
@end type
"#;
        let iface = parse_amod(amod_text, Path::new("test.amod")).unwrap();
        assert_eq!(iface.module_name, "physics");
        assert_eq!(
            iface.dependencies,
            vec![AmodModuleDependency {
                module_name: "iso_c_binding".into(),
                nature: UseNature::Normal,
            }]
        );
        assert!(iface.only_imports.is_empty());
        assert_eq!(iface.variables.len(), 2); // call_count + gravity
        assert!(iface.variables.iter().any(|v| v.name == "call_count"
            && v.ir_symbol.as_deref() == Some("afs_mod_physics_call_count")));
        assert!(iface
            .variables
            .iter()
            .any(|v| v.name == "gravity" && v.is_parameter));
        assert!(iface.variables.iter().all(|v| !v.proc_pointer));
        assert_eq!(iface.procedures.len(), 2);
        let ke = iface
            .procedures
            .iter()
            .find(|p| p.name == "kinetic_energy")
            .unwrap();
        assert!(ke.pure);
        assert_eq!(ke.args.len(), 2);
        assert_eq!(ke.args[0].name, "self");
        assert!(matches!(ke.args[0].intent, Some(Intent::In)));
        let af = iface
            .procedures
            .iter()
            .find(|p| p.name == "apply_force")
            .unwrap();
        assert_eq!(af.args.len(), 2);
        assert!(af.args[1].optional);
        assert_eq!(iface.types.len(), 1);
        let pt = &iface.types[0].layout;
        assert_eq!(iface.types[0].access, Access::Public);
        assert_eq!(pt.name, "particle");
        assert_eq!(pt.size, 12);
        assert_eq!(pt.fields.len(), 3);
        assert_eq!(pt.bound_procs.len(), 1);
        assert_eq!(pt.bound_procs[0].method_name, "kinetic_energy");
        assert_eq!(
            pt.final_procs,
            vec![crate::sema::type_layout::FinalProc {
                name: "afs_modproc_physics_finish_particles".into(),
                rank: 1,
                elemental: false,
            }]
        );
    }

    #[test]
    fn current_default_access_header_is_required_and_preserved() {
        let private = r#"#!amod 19
# module: private_facade
# default-access: private

@uses provider
@access private :: hidden
@access public :: selected
"#;
        let iface = parse_amod(private, Path::new("private_facade.amod")).unwrap();
        assert_eq!(iface.default_access, Access::Private);
        assert_eq!(
            iface.named_access,
            vec![
                ("hidden".into(), Access::Private),
                ("selected".into(), Access::Public)
            ]
        );

        let missing = r#"#!amod 19
# module: missing_default_access

@uses provider
"#;
        let error = parse_amod(missing, Path::new("missing_default_access.amod"))
            .expect_err("current .amod without module default access was accepted");
        assert!(
            error.contains("corrupt .amod file")
                && error.contains("missing default-access header")
                && error.contains("rebuild the provider module"),
            "unexpected missing-default-access diagnostic: {error}"
        );

        let legacy = r#"#!amod 12
# module: legacy_default_access

@uses provider
"#;
        let iface = parse_amod(legacy, Path::new("legacy_default_access.amod")).unwrap();
        assert_eq!(iface.default_access, Access::Public);
    }

    #[test]
    fn dotted_relational_operator_interface_loads_with_canonical_name() {
        let text = r#"#!amod 19
# module: operators
# default-access: public

@interface operator(.eq.)
  @specific eq_mixed
@end interface
"#;
        let iface = parse_amod(text, Path::new("operators.amod")).unwrap();
        assert_eq!(iface.interfaces.len(), 1);
        assert_eq!(iface.interfaces[0].name, "operator(==)");
    }

    #[test]
    fn current_type_records_require_and_preserve_accessibility() {
        let amod_text = r#"#!amod 12
# module: type_access

@type exposed_t, public
  @layout size=4 align=4
  @field value : integer @offset 0 @size 4 @access public @owner type_access
@end type

@type hidden_t, private
  @layout size=4 align=4
  @field value : integer @offset 0 @size 4 @access public @owner type_access
@end type
"#;
        let iface = parse_amod(amod_text, Path::new("type_access.amod")).unwrap();
        assert_eq!(iface.types.len(), 2);
        assert_eq!(iface.types[0].layout.name, "exposed_t");
        assert_eq!(iface.types[0].access, Access::Public);
        assert_eq!(iface.types[0].layout.fields[0].access, Access::Public);
        assert_eq!(
            iface.types[0].layout.fields[0].owner_module.as_deref(),
            Some("type_access")
        );
        assert_eq!(iface.types[1].layout.name, "hidden_t");
        assert_eq!(iface.types[1].access, Access::Private);

        let missing_access = r#"#!amod 12
# module: missing_type_access

@type hidden_t
  @layout size=4 align=4
@end type
"#;
        let err = parse_amod(missing_access, Path::new("missing_type_access.amod"))
            .expect_err("current @type records without accessibility must be rejected");
        assert!(
            err.contains("corrupt .amod file")
                && err.contains("malformed @type accessibility")
                && err.contains("rebuild the provider module"),
            "unexpected missing-access diagnostic: {err}"
        );

        for (record, expected) in [
            (
                "@field value : integer @offset 0 @size 4 @owner type_access",
                "missing @field accessibility",
            ),
            (
                "@field value : integer @offset 0 @size 4 @access protected @owner type_access",
                "invalid @field accessibility",
            ),
            (
                "@field value : integer @offset 0 @size 4 @access private",
                "missing @field owner",
            ),
            (
                "@field value : integer @offset 0 @size 4 @access public @access private @owner type_access",
                "duplicate @field accessibility",
            ),
            (
                "@field value : integer @offset 0 @size 4 @access public @owner type_access @owner type_access",
                "duplicate @field owner",
            ),
            (
                "@field value : integer @offset 0 @size 4 @access public @owner @pointer",
                "invalid @field owner",
            ),
        ] {
            let malformed = format!(
                "#!amod 12\n# module: type_access\n\n@type item_t, public\n  @layout size=4 align=4\n  {record}\n@end type\n"
            );
            let error = parse_amod(&malformed, Path::new("bad_field_access.amod"))
                .expect_err("malformed current @field access was accepted");
            assert!(
                error.contains("corrupt .amod file")
                    && error.contains(expected)
                    && error.contains("rebuild the provider module"),
                "unexpected malformed-field diagnostic: {error}"
            );
        }

        let legacy = r#"#!amod 9
# module: legacy_type_access

@type item_t, public
  @layout size=4 align=4
  @field value : integer @offset 0 @size 4
  @owner legacy_type_access
@end type
"#;
        let legacy_iface = parse_amod(legacy, Path::new("legacy_type_access.amod")).unwrap();
        assert_eq!(
            legacy_iface.types[0].layout.fields[0].access,
            Access::Public
        );
        assert_eq!(
            legacy_iface.types[0].layout.fields[0]
                .owner_module
                .as_deref(),
            Some("legacy_type_access")
        );
    }

    #[test]
    fn only_qualified_dependencies_round_trip_exact_bindings() {
        let amod_text = r#"#!amod 8
# module: facade
# source: facade.f90

@uses intrinsic :: intrinsic_dep
@uses non_intrinsic :: authored_dep
@uses legacy_dep
@use_only visible = visible from intrinsic :: filtered_dep
@use_only alias = remote from non_intrinsic :: filtered_dep
@use_rename local_name = remote_name from bare_dep
"#;
        let iface = parse_amod(amod_text, Path::new("test.amod")).unwrap();
        assert_eq!(
            iface.dependencies,
            vec![
                AmodModuleDependency {
                    module_name: "intrinsic_dep".into(),
                    nature: UseNature::Intrinsic,
                },
                AmodModuleDependency {
                    module_name: "authored_dep".into(),
                    nature: UseNature::NonIntrinsic,
                },
                AmodModuleDependency {
                    module_name: "legacy_dep".into(),
                    nature: UseNature::Normal,
                },
            ]
        );
        assert_eq!(
            iface.only_imports,
            vec![
                UseRename {
                    local: "visible".into(),
                    original: "visible".into(),
                    source_module: "filtered_dep".into(),
                    source_nature: UseNature::Intrinsic,
                },
                UseRename {
                    local: "alias".into(),
                    original: "remote".into(),
                    source_module: "filtered_dep".into(),
                    source_nature: UseNature::NonIntrinsic,
                },
            ]
        );
        assert_eq!(
            iface.renames,
            vec![UseRename {
                local: "local_name".into(),
                original: "remote_name".into(),
                source_module: "bare_dep".into(),
                source_nature: UseNature::Normal,
            }]
        );
    }

    #[test]
    fn malformed_use_bindings_are_rejected() {
        for record in [
            "@use_only",
            "@use_only local = remote",
            "@use_only = remote from provider",
            "@use_rename",
            "@use_rename local = from provider",
        ] {
            let amod_text =
                format!("#!amod 7\n# module: facade\n# source: facade.f90\n\n{record}\n");
            let err = parse_amod(&amod_text, Path::new("bad.amod")).unwrap_err();
            assert!(
                err.contains("corrupt .amod file")
                    && err.contains("malformed @use_")
                    && err.contains("rebuild the provider module"),
                "unexpected error for {record}: {err}"
            );
        }
    }

    #[test]
    fn legacy_final_proc_characteristics_are_inferred_from_procedure() {
        let amod_text = r#"#!amod 2
# module: m

@subroutine finish_array
  @arg values : type(array_item), rank=1
@end subroutine

@subroutine finish_elemental, elemental
  @arg value : type(scalar_item)
@end subroutine

@type array_item
  @final afs_modproc_m_finish_array
@end type

@type scalar_item
  @final afs_modproc_m_finish_elemental
@end type
"#;
        let iface = parse_amod(amod_text, Path::new("legacy.amod")).unwrap();
        assert_eq!(iface.types[0].layout.final_procs[0].rank, 1);
        assert!(!iface.types[0].layout.final_procs[0].elemental);
        assert_eq!(iface.types[1].layout.final_procs[0].rank, 0);
        assert!(iface.types[1].layout.final_procs[0].elemental);
    }

    #[test]
    fn current_final_proc_elemental_metadata_is_required_and_preserved() {
        let amod_text = r#"#!amod 12
# module: m

@type scalar_t, public
  @final afs_modproc_m_finish_scalar rank=0 elemental=false
@end type

@type elemental_t, public
  @final afs_modproc_m_finish_elemental rank=0 elemental=true
@end type
"#;
        let iface = parse_amod(amod_text, Path::new("current.amod")).unwrap();
        assert!(!iface.types[0].layout.final_procs[0].elemental);
        assert!(iface.types[1].layout.final_procs[0].elemental);

        let missing = r#"#!amod 12
# module: m

@type item_t, public
  @final afs_modproc_m_finish rank=0
@end type
"#;
        let err = parse_amod(missing, Path::new("missing.amod"))
            .expect_err("current @final records must carry elemental metadata");
        assert!(
            err.contains("missing @final elemental metadata")
                && err.contains("rebuild the provider module"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn malformed_final_proc_rank_is_rejected() {
        let mut lines = "  @final afs_modproc_m_finish rank=oops\n@end type\n"
            .lines()
            .peekable();
        let error = parse_type("@type item", &mut lines, 2, Path::new("legacy.amod"))
            .expect_err("malformed @final rank was accepted");
        assert!(error.contains("malformed @final rank 'oops'"), "{error}");
    }

    #[test]
    fn proc_pointer_var_round_trips_with_global_storage() {
        let amod_text = r#"#!amod 2
# module: control_flow
# source: control_flow.f90
# checksum: sha256:def456

@subroutine evaluate_condition_interface
  @abi cc=aapcs64 hidden_char_lens=0
  @arg n : integer, intent(inout)
    @abi pass=x0 width=8
@end subroutine

@var evaluate_condition : type(evaluate_condition_interface), pointer, procptr @ir afs_mod_control_flow_evaluate_condition
"#;
        let iface = parse_amod(amod_text, Path::new("test.amod")).unwrap();
        let var = iface
            .variables
            .iter()
            .find(|v| v.name == "evaluate_condition")
            .unwrap();
        assert!(var.pointer);
        assert!(var.proc_pointer);
        assert!(matches!(
            var.type_info,
            Some(TypeInfo::Derived(ref name)) if name == "evaluate_condition_interface"
        ));

        let globals = extract_module_globals(&iface);
        let info = globals
            .get(&("control_flow".into(), "evaluate_condition".into()))
            .unwrap();
        assert!(info.is_pointer);
        assert_eq!(info.symbol, "afs_mod_control_flow_evaluate_condition");
        assert_eq!(
            info.ty,
            crate::ir::types::IrType::Ptr(Box::new(crate::ir::types::IrType::Int(
                crate::ir::types::IntWidth::I8
            )))
        );
    }

    #[test]
    fn arg_rank_round_trips_for_array_dummies() {
        // Sprint35-SMP Phase 1: the rank=N attribute on @arg lines must
        // round-trip so the consumer can rebuild a SymbolAttrs::array_spec
        // of the right rank for SMP-body synthesis. Scalar args (no
        // rank=) parse as rank 0; descriptor-passed assumed-shape arrays
        // carry their rank.
        let amod_text = r#"#!amod 2
# module: shapes
# source: shapes.f90
# checksum: sha256:abc

@subroutine takes_assumed_shape
  @abi cc=aapcs64 hidden_char_lens=0
  @arg a : real, intent(in), descriptor, rank=1
    @abi pass=x0 width=8
  @arg b : real, intent(in), descriptor, rank=2
    @abi pass=x1 width=8
  @arg n : integer, intent(in)
    @abi pass=x2 width=8
@end subroutine

@subroutine takes_alloc_array
  @abi cc=aapcs64 hidden_char_lens=0
  @arg buf : real, intent(out), descriptor, allocatable, rank=1
    @abi pass=x0 width=8
@end subroutine

@subroutine takes_assumed_rank
  @abi cc=aapcs64 hidden_char_lens=0
  @arg value : class(*), intent(in), descriptor, assumed-rank, rank=1
    @abi pass=x0 width=8
@end subroutine
"#;
        let iface = parse_amod(amod_text, Path::new("test.amod")).unwrap();
        let assumed = iface
            .procedures
            .iter()
            .find(|p| p.name == "takes_assumed_shape")
            .unwrap();
        assert_eq!(assumed.args[0].rank, 1);
        assert_eq!(assumed.args[1].rank, 2);
        assert_eq!(assumed.args[2].rank, 0); // scalar n: no rank= attribute

        let alloc = iface
            .procedures
            .iter()
            .find(|p| p.name == "takes_alloc_array")
            .unwrap();
        assert_eq!(alloc.args[0].rank, 1);
        assert!(alloc.args[0].allocatable);

        let assumed_rank = iface
            .procedures
            .iter()
            .find(|p| p.name == "takes_assumed_rank")
            .unwrap();
        assert_eq!(assumed_rank.args[0].rank, 1);
        assert!(assumed_rank.args[0].assumed_rank);
    }

    #[test]
    fn procedure_characteristic_metadata_round_trips() {
        let arg = parse_arg(
            "@arg values : integer(4), intent(inout), optional, value, descriptor, \
             allocatable, pointer, target, asynchronous, contiguous, volatile, \
             external, procedure(callback), rank=6, \
             procedure_original=remote_callback, procedure_module=callback_shapes, \
             procedure_nature=non_intrinsic, \
             array_spec=\"(count; 0:max(count, 1); :; 1:*; deferred; ..)\"",
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert_eq!(arg.intent, Some(Intent::InOut));
        assert!(arg.optional);
        assert!(arg.value);
        assert!(arg.descriptor);
        assert!(arg.allocatable);
        assert!(arg.pointer);
        assert!(arg.target);
        assert!(arg.asynchronous);
        assert!(arg.contiguous);
        assert!(arg.volatile);
        assert!(arg.external);
        assert_eq!(arg.procedure_iface.as_deref(), Some("callback"));
        let arg_import = arg.procedure_iface_import.as_ref().unwrap();
        assert_eq!(arg_import.local, "callback");
        assert_eq!(arg_import.original, "remote_callback");
        assert_eq!(arg_import.source_module, "callback_shapes");
        assert_eq!(arg_import.source_nature, UseNature::NonIntrinsic);
        assert_eq!(arg.rank, 6);
        assert!(matches!(
            arg.array_spec.as_deref(),
            Some([
                crate::ast::decl::ArraySpec::Explicit { lower: None, .. },
                crate::ast::decl::ArraySpec::Explicit { lower: Some(_), .. },
                crate::ast::decl::ArraySpec::AssumedShape { lower: None },
                crate::ast::decl::ArraySpec::AssumedSize { lower: Some(_) },
                crate::ast::decl::ArraySpec::Deferred,
                crate::ast::decl::ArraySpec::AssumedRank,
            ])
        ));

        let mut lines = "@end function\n".lines().peekable();
        let procedure = parse_proc(
            "@function factory -> type(callback), result_pointer, \
             result_procedure=callback, result_procedure_original=remote_callback, \
             result_procedure_module=callback_shapes, \
             result_procedure_nature=non_intrinsic, result_name=result",
            &mut lines,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert!(procedure.result_pointer);
        assert!(!procedure.abstract_interface);
        assert_eq!(
            procedure.result_procedure_iface.as_deref(),
            Some("callback")
        );
        let result_import = procedure.result_procedure_iface_import.as_ref().unwrap();
        assert_eq!(result_import.local, "callback");
        assert_eq!(result_import.original, "remote_callback");
        assert_eq!(result_import.source_module, "callback_shapes");
        assert_eq!(result_import.source_nature, UseNature::NonIntrinsic);
        assert_eq!(procedure.result_name.as_deref(), Some("result"));

        let mut lines = "@end subroutine\n".lines().peekable();
        let abstract_procedure = parse_proc(
            "@subroutine callback, abstract_interface",
            &mut lines,
            AMOD_VERSION,
            Path::new("test.amod"),
        )
        .unwrap();
        assert!(abstract_procedure.abstract_interface);
    }

    #[test]
    fn amod_cache_skips_reparse_on_second_read() {
        // Write a small valid .amod, read it twice, and verify the
        // hit counter advanced exactly once.
        clear_amod_cache();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("amod_cache_test_{}.amod", std::process::id()));
        let text = add_integrity_headers(
            r#"#!amod 19
# module: cache_test
# default-access: public
# source: cache_test.f90

@param k : integer(4) = 7
"#
            .to_string(),
        );
        std::fs::write(&path, text).unwrap();

        let _ = read_amod(&path).expect("first read");
        let hits_after_first = AMOD_CACHE_HITS.with(|h| *h.borrow());

        let _ = read_amod(&path).expect("second read");
        let hits_after_second = AMOD_CACHE_HITS.with(|h| *h.borrow());

        let _ = std::fs::remove_file(&path);
        assert_eq!(hits_after_first, 0, "first read must miss the cache");
        assert_eq!(
            hits_after_second, 1,
            "second read must hit the cache (no reparse)"
        );
    }

    #[test]
    fn supplied_amod_bytes_preserve_submodule_contracts_without_cache_lookup() {
        clear_amod_cache();
        let path = std::env::temp_dir().join(format!(
            "amod_supplied_content_test_{}.amod",
            std::process::id()
        ));
        let cached_text = add_integrity_headers(
            r#"#!amod 19
# module: cached_parent
# default-access: public
# source: cached_parent.f90

@param cached : integer(4) = 1
"#
            .to_string(),
        );
        std::fs::write(&path, cached_text).unwrap();
        let cached = read_amod(&path).expect("cache seed read");
        assert_eq!(cached.module_name, "cached_parent");

        let supplied_text = add_integrity_headers(
            r#"#!amod 19
# module: supplied_parent
# default-access: private
# ancestor-module: supplied_root
# parent-submodule: supplied_middle
# host-association: only kept local_value
# source: supplied_parent.f90

@function implemented -> integer(4), module_procedure
@end function
"#
            .to_string(),
        );
        let supplied =
            read_amod_content(&supplied_text, &path).expect("supplied content should parse");

        let _ = std::fs::remove_file(&path);
        assert_eq!(supplied.module_name, "supplied_parent");
        assert_eq!(supplied.default_access, Access::Private);
        assert_eq!(
            supplied.host_association,
            AmodHostAssociation::Only(vec!["kept".into(), "local_value".into()])
        );
        assert!(supplied.procedures[0].is_separate_module_procedure);
        assert_eq!(
            AMOD_CACHE_HITS.with(|hits| *hits.borrow()),
            0,
            "parsing supplied bytes must not consult the path cache"
        );
    }

    #[test]
    fn ancestor_owned_procedure_metadata_uses_root_link_name() {
        let text = add_integrity_headers(
            r#"#!amod 19
# module: child
# default-access: public
# ancestor-module: root
# parent-submodule: middle
# source: child.f90

@subroutine compute, module_procedure
  @arg values : integer(4), intent(in), descriptor, rank=1
    @abi pass=x0 width=8
  @arg text : character(len=:,kind=1), intent(in)
    @abi pass=x1 width=8
  @arg bias : integer(4), intent(in), optional
    @abi pass=x2 width=8
  @arg text@len : integer(8), hidden
    @abi pass=x3 width=8
@end subroutine

@subroutine required, module_procedure
  @arg value : integer(4), intent(in)
    @abi pass=x0 width=8
@end subroutine
"#
            .to_string(),
        );
        let iface = read_amod_content(&text, Path::new("child.amod")).unwrap();
        let root_key = "afs_modproc$v2$m$root$p$compute";

        assert_eq!(
            extract_optional_params(&iface).get(root_key),
            Some(&vec![false, false, true])
        );
        assert_eq!(
            extract_descriptor_params(&iface).get(root_key),
            Some(&vec![true, false, false])
        );
        assert_eq!(
            extract_char_len_star_params(&iface).get(root_key),
            Some(&vec![false, true, false])
        );
        assert_eq!(
            extract_optional_params(&iface).get("afs_modproc$v2$m$root$p$required"),
            Some(&vec![false])
        );
    }

    #[test]
    fn read_amod_rejects_truncated_integrity_body() {
        clear_amod_cache();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("amod_truncated_test_{}.amod", std::process::id()));
        let text = add_integrity_headers(
            r#"#!amod 7
# module: truncated_test
# source: truncated_test.f90

@param k : integer = 7
@param answer : integer = 42
"#
            .to_string(),
        )
        .replace("@param answer : integer = 42\n", "");
        std::fs::write(&path, text).unwrap();

        let err = read_amod(&path).expect_err("truncated .amod must be rejected");

        let _ = std::fs::remove_file(&path);
        assert_eq!(
            AMOD_CACHE_HITS.with(|h| *h.borrow()),
            0,
            "rejected .amod must not populate the cache"
        );
        assert!(
            err.contains("corrupt .amod file") && err.contains("content length"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn read_amod_rejects_stale_abi_version() {
        clear_amod_cache();
        let path = std::env::temp_dir().join(format!(
            "amod_stale_version_test_{}.amod",
            std::process::id()
        ));
        let text = add_integrity_headers(
            r#"#!amod 18
# module: stale_test
# source: stale_test.f90

@param k : integer = 7
"#
            .to_string(),
        );
        std::fs::write(&path, text).unwrap();

        let err = read_amod(&path).expect_err("stale .amod must be rejected");

        let _ = std::fs::remove_file(&path);
        assert!(
            err.contains("incompatible .amod version 18 (compiler requires 19)")
                && err.contains("rebuild the provider module"),
            "unexpected error: {err}"
        );
    }
}
