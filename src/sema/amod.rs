//! ARMFORTAS module file (.amod) v2 writer and reader.
//!
//! Format spec: see `.claude/plans/composed-questing-catmull.md`.
//!
//! The `.amod` file is a human-readable, self-documenting, diffable
//! description of a Fortran module's public interface — carrying
//! enough information for full ABI-correct separate compilation.
//!
//! Innovations over gfortran/flang/ifort:
//!   - Explicit ABI annotations (@abi with register assignments)
//!   - Optimization hints (@hint leaf, no_globals, cost)
//!   - Linker symbol names (@ir) for direct FFI
//!   - Source checksum for staleness detection
//!   - Polymorphic type tags (@tag)
//!   - Human-editable for hand-written FFI descriptions

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::path::Path;

use crate::ir::inst::{FuncRef, Function, InstKind, Module as IrModule};
use crate::ir::lower::ModuleGlobalInfo;
use crate::sema::symtab::*;
use crate::sema::type_layout::TypeLayoutRegistry;

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
        // Find the first `:` at depth 0 (parens/brackets) to split
        // lower:upper. Don't split on `:` inside function calls.
        let mut depth: i32 = 0;
        let mut split_at: Option<usize> = None;
        for (idx, ch) in dim.char_indices() {
            match ch {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                ':' if depth == 0 => {
                    split_at = Some(idx);
                    break;
                }
                _ => {}
            }
        }
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
        FieldDefaultInit::Character(value) => format!("C{}", hex_encode_bytes(value.as_bytes())),
        FieldDefaultInit::Integer(value) => format!("I{}", value),
        FieldDefaultInit::Logical(value) => format!("L{}", if *value { '1' } else { '0' }),
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
        let decoded = String::from_utf8(hex_decode_bytes(value)?).ok()?;
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

/// Serialize a module's public interface to `.amod` v2 text.
pub fn write_amod(
    module_name: &str,
    source_path: &str,
    source_content: &str,
    st: &SymbolTable,
    mod_scope_id: ScopeId,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    type_layouts: &TypeLayoutRegistry,
    ir_module: &IrModule,
    descriptor_params: &HashMap<String, Vec<bool>>,
    char_len_star_params: &HashMap<String, Vec<bool>>,
) -> String {
    let mut out = String::new();
    let mod_key = module_name.to_lowercase();
    let scope = st.scope(mod_scope_id);

    // ---- Header ----
    writeln!(out, "#!amod 2").unwrap();
    writeln!(out, "# module: {}", mod_key).unwrap();
    writeln!(out, "# source: {}", source_path).unwrap();
    writeln!(out, "# checksum: fnv1a:{}", fnv1a_hex(source_content)).unwrap();
    writeln!(out, "# compiled: {}", compile_timestamp()).unwrap();
    writeln!(out, "# compiler: armfortas 0.1.0").unwrap();
    writeln!(out, "# abi: arm64-apple-darwin").unwrap();
    writeln!(out).unwrap();

    // ---- Dependencies ----
    let mut deps: Vec<String> = scope
        .use_associations
        .iter()
        .filter_map(|ua| {
            let src_scope = st.scope(ua.source_scope);
            if let ScopeKind::Module(ref n) = src_scope.kind {
                Some(n.to_lowercase())
            } else {
                None
            }
        })
        .collect();
    deps.sort();
    deps.dedup();
    for dep in &deps {
        writeln!(out, "@uses {}", dep).unwrap();
    }
    if !deps.is_empty() {
        writeln!(out).unwrap();
    }

    // ---- Use renames ----
    // Record each `use M, only: a => b` as `@use_rename a = b from m`.
    // Submodule bodies pulled in by host association need to resolve
    // names like `block_kind` (renamed from `int64`) for kind selectors
    // and intrinsic dispatch; without preserving the rename, the .amod
    // can't reconstruct the kind constant and `integer(block_kind) ::
    // dummy` falls back to the default kind.
    let mut renames_out: Vec<(String, String, String)> = scope
        .use_associations
        .iter()
        .filter_map(|ua| {
            if ua.local_name == ua.original_name {
                return None;
            }
            let src_scope = st.scope(ua.source_scope);
            if let ScopeKind::Module(ref n) = src_scope.kind {
                Some((
                    ua.local_name.clone(),
                    ua.original_name.clone(),
                    n.to_lowercase(),
                ))
            } else {
                None
            }
        })
        .collect();
    renames_out.sort();
    renames_out.dedup();
    for (local, original, src) in &renames_out {
        writeln!(out, "@use_rename {} = {} from {}", local, original, src).unwrap();
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
        emit_variable(&mut out, &mod_key, name, sym, globals);
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
        emit_parameter(&mut out, &mod_key, name, sym, globals);
    }
    if !params.is_empty() {
        writeln!(out).unwrap();
    }

    // ---- Procedures ----
    let interface_specifics: BTreeSet<String> = syms
        .iter()
        .filter(|(_, sym)| {
            matches!(sym.kind, SymbolKind::NamedInterface)
                || (matches!(sym.kind, SymbolKind::DerivedType) && !sym.arg_names.is_empty())
        })
        .flat_map(|(_, sym)| sym.arg_names.iter().cloned())
        .collect();
    // Public derived types can expose private bound procedure targets across
    // translation units. Those targets must be serialized too so imported
    // type-bound calls can recover full dummy-argument ABI metadata such as
    // OPTIONAL slots.
    let mut proc_export_names: BTreeSet<String> = interface_specifics;
    for (name, sym) in &syms {
        if matches!(sym.kind, SymbolKind::Function | SymbolKind::Subroutine)
            && is_public(sym, scope)
        {
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
        if let Some(layout) = type_layouts.get(key) {
            emit_type(&mut out, &layout.name, type_layouts);
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

    out
}

fn is_public(sym: &Symbol, scope: &Scope) -> bool {
    match sym.attrs.access {
        Access::Private => false,
        Access::Public => true,
        Access::Default => !matches!(scope.default_access, Access::Private),
    }
}

fn emit_variable(
    out: &mut String,
    mod_key: &str,
    name: &str,
    sym: &Symbol,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
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
        Some(TypeInfo::Character { len: None, .. }),
        Some(ModuleGlobalInfo {
            char_kind: crate::ir::lower::CharKind::Fixed(n),
            ..
        }),
    ) = (sym.type_info.as_ref(), global_info)
    {
        format!("character(len={})", n)
    } else {
        type_info_to_string(sym.type_info.as_ref())
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
) {
    let global_key = (mod_key.to_string(), name.to_lowercase());
    let global_info = globals.get(&global_key);
    let type_str = if let (
        Some(TypeInfo::Character { len: None, .. }),
        Some(ModuleGlobalInfo {
            char_kind: crate::ir::lower::CharKind::Fixed(n),
            ..
        }),
    ) = (sym.type_info.as_ref(), global_info)
    {
        format!("character(len={})", n)
    } else {
        type_info_to_string(sym.type_info.as_ref())
    };
    let is_private = sym.attrs.access == Access::Private;
    if let Some(cv) = sym.const_value {
        // Place `, private` after the value so parse_var's
        // rfind(" = ") inside type_str continues to work.
        let suf = if is_private { ", private" } else { "" };
        writeln!(out, "@param {} : {} = {}{}", name, type_str, cv, suf).unwrap();
    } else if let Some(info) = global_info {
        // For @ir-backed params, attach `, private` to the type so
        // the parser sees it in attr_str rather than after @ir.
        let type_with_attr = if is_private {
            format!("{}, private", type_str)
        } else {
            type_str
        };
        writeln!(
            out,
            "@param {} : {} @ir {}",
            name, type_with_attr, info.symbol
        )
        .unwrap();
    } else {
        let suf = if is_private { ", private" } else { "" };
        writeln!(out, "@param {} : {}{}", name, type_str, suf).unwrap();
    }
}

fn emit_procedure(
    out: &mut String,
    name: &str,
    sym: &Symbol,
    st: &SymbolTable,
    mod_scope_id: ScopeId,
    ir_module: &IrModule,
    descriptor_params: &HashMap<String, Vec<bool>>,
    _char_len_star_params: &HashMap<String, Vec<bool>>,
) {
    let is_func = matches!(sym.kind, SymbolKind::Function);
    let kind_str = if is_func { "function" } else { "subroutine" };

    if is_func {
        let ret_str = type_info_to_string(sym.type_info.as_ref());
        write!(out, "@function {} -> {}", sym.name, ret_str).unwrap();
        if sym.attrs.allocatable {
            write!(out, ", result_allocatable").unwrap();
        }
        if sym.attrs.pointer {
            write!(out, ", result_pointer").unwrap();
        }
        if sym.attrs.result_rank > 0 {
            write!(out, ", result_rank={}", sym.attrs.result_rank).unwrap();
        }
        // Sprint35-SMP Phase 2: emit the result variable's user-declared
        // name when it differs from the function name (i.e. the source
        // had a `result(X)` clause). Submodule bodies that reference the
        // result by its declared name need this preserved across the
        // .amod boundary so sema can register the right symbol.
        let result_var_name: Option<String> = st
            .scopes
            .iter()
            .find(|s| {
                let matches_name = match &s.kind {
                    ScopeKind::Function(n) | ScopeKind::Subroutine(n) => {
                        n.eq_ignore_ascii_case(name)
                    }
                    _ => false,
                };
                if !matches_name {
                    return false;
                }
                let Some(parent_id) = s.parent else {
                    return false;
                };
                parent_id == mod_scope_id
                    || matches!(st.scope(parent_id).kind, ScopeKind::Interface)
                        && st.scope(parent_id).parent == Some(mod_scope_id)
            })
            .and_then(|pscope| {
                let arg_set: std::collections::HashSet<String> =
                    pscope.arg_order.iter().map(|n| n.to_lowercase()).collect();
                pscope
                    .symbols
                    .iter()
                    .find(|(key, sym)| {
                        !arg_set.contains(*key)
                            && matches!(sym.kind, SymbolKind::Variable | SymbolKind::Parameter)
                    })
                    .map(|(_, sym)| sym.name.clone())
            });
        if let Some(result_var_name) = result_var_name {
            if !result_var_name.eq_ignore_ascii_case(name) {
                write!(out, ", result_name={}", result_var_name).unwrap();
            }
        }
        // Sprint35-SMP Phase 3: serialize the result variable's
        // explicit-shape bounds so split-file submodule bodies (where
        // the body's TU loads the parent module from .amod) can rebuild
        // a same-shape ArraySpec at load time. Without this, the body's
        // `res(i) = …` lowers against an AssumedShape result and the
        // function prologue fails to allocate the runtime-shape buffer.
        if !sym.attrs.allocatable && !sym.attrs.pointer && sym.attrs.result_rank > 0 {
            let bounds = st
                .scopes
                .iter()
                .find(|s| {
                    let matches_name = match &s.kind {
                        ScopeKind::Function(n) | ScopeKind::Subroutine(n) => {
                            n.eq_ignore_ascii_case(name)
                        }
                        _ => false,
                    };
                    if !matches_name {
                        return false;
                    }
                    let Some(parent_id) = s.parent else {
                        return false;
                    };
                    parent_id == mod_scope_id
                        || matches!(st.scope(parent_id).kind, ScopeKind::Interface)
                            && st.scope(parent_id).parent == Some(mod_scope_id)
                })
                .and_then(|pscope| {
                    let arg_set: std::collections::HashSet<String> = pscope
                        .arg_order
                        .iter()
                        .map(|n| n.to_lowercase())
                        .collect();
                    pscope
                        .symbols
                        .iter()
                        .find(|(key, sym)| {
                            !arg_set.contains(*key)
                                && matches!(
                                    sym.kind,
                                    SymbolKind::Variable | SymbolKind::Parameter
                                )
                        })
                        .map(|(_, sym)| sym.attrs.array_spec.clone())
                })
                .and_then(|specs| stringify_array_bounds(&specs));
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
    if sym.attrs.access == Access::Private {
        write!(out, ", private").unwrap();
    }
    if let Some(binding_label) = &sym.attrs.binding_label {
        write!(out, ", bind={}", binding_label).unwrap();
    }
    writeln!(out).unwrap();

    let name_lc = name.to_lowercase();
    let ir_func = ir_module.functions.iter().find(|f| {
        f.name.eq_ignore_ascii_case(name)
            || f.name.eq_ignore_ascii_case(&name_lc)
            || f.name.to_lowercase().ends_with(&format!("_{}", name_lc))
    });
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

    // Walk into the procedure's scope for full arg info. Interface-declared
    // procedures sit under an intermediate Interface scope rather than
    // directly under the module, so check both shapes.
    let proc_scope = st
        .scopes
        .iter()
        .find(|s| {
            s.parent == Some(mod_scope_id)
                && match &s.kind {
                    ScopeKind::Function(n) | ScopeKind::Subroutine(n) => {
                        n.eq_ignore_ascii_case(name)
                    }
                    _ => false,
                }
        })
        .or_else(|| {
            st.scopes.iter().find(|s| {
                let matches_name = match &s.kind {
                    ScopeKind::Function(n) | ScopeKind::Subroutine(n) => {
                        n.eq_ignore_ascii_case(name)
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
                matches!(parent.kind, ScopeKind::Interface) && parent.parent == Some(mod_scope_id)
            })
        });

    let is_bind_c = sym.attrs.binding_label.is_some();
    let declared_descriptor_params = descriptor_params.get(&name.to_lowercase());

    // Compute hidden char-length count from the scope's arg types.
    let mut hidden_count = 0usize;
    if let Some(pscope) = proc_scope {
        for arg_name in &pscope.arg_order {
            if let Some(arg_sym) = pscope.symbols.get(&arg_name.to_lowercase()) {
                if matches!(
                    arg_sym.type_info,
                    Some(TypeInfo::Character { len: None, .. })
                ) && !arg_sym.attrs.allocatable
                    && !is_bind_c
                {
                    hidden_count += 1;
                }
            }
        }
    }

    // @abi line for the procedure.
    writeln!(out, "  @abi cc=aapcs64 hidden_char_lens={}", hidden_count).unwrap();

    let mut reg_idx = 0usize;
    if let Some(pscope) = proc_scope {
        for (arg_idx, arg_name) in pscope.arg_order.iter().enumerate() {
            if let Some(arg_sym) = pscope.symbols.get(&arg_name.to_lowercase()) {
                let type_str = type_info_to_string(arg_sym.type_info.as_ref());
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
                                    crate::ir::types::IrType::Array(elem, 384)
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
                    || matches!(arg_sym.type_info, Some(TypeInfo::Class(_)) | Some(TypeInfo::ClassStar));
                if is_descriptor_arg {
                    arg_attrs.push("descriptor");
                }
                if arg_sym.attrs.allocatable {
                    arg_attrs.push("allocatable");
                }
                if arg_sym.attrs.pointer {
                    arg_attrs.push("pointer");
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
                // Sprint35-SMP Phase 1: emit the dummy's rank so SMP-body
                // synthesis on the consumer side can rebuild a same-rank
                // array_spec without re-walking the AST decls (which only
                // exist on the producer side at .amod write time).
                let rank_attr = format!("rank={}", arg_sym.attrs.array_spec.len());
                if !arg_sym.attrs.array_spec.is_empty() {
                    arg_attrs.push(rank_attr.as_str());
                }
                if !arg_attrs.is_empty() {
                    write!(out, ", {}", arg_attrs.join(", ")).unwrap();
                }
                writeln!(out).unwrap();
                // @abi per arg — ARM64 AAPCS64: first 8 int/ptr args in x0-x7.
                let reg = if reg_idx < 8 {
                    format!("x{}", reg_idx)
                } else {
                    format!("stack+{}", (reg_idx - 8) * 8)
                };
                writeln!(out, "    @abi pass={} width=8", reg).unwrap();
                reg_idx += 1;
            } else {
                writeln!(out, "  @arg {}", arg_name).unwrap();
                reg_idx += 1;
            }
        }
    } else {
        // Fallback: use arg_names from the symbol (no type info).
        for arg_name in &sym.arg_names {
            writeln!(out, "  @arg {}", arg_name).unwrap();
            reg_idx += 1;
        }
    }

    // Hidden character-length args — infer from the scope's arg types.
    // Any arg with TypeInfo::Character { len: None } that isn't
    // allocatable is an assumed-length (len=*) dummy that gets a
    // hidden i64 length parameter appended after the normal args.
    if let Some(pscope) = proc_scope {
        for arg_name in &pscope.arg_order {
            if let Some(arg_sym) = pscope.symbols.get(&arg_name.to_lowercase()) {
                let is_assumed_len = matches!(
                    arg_sym.type_info,
                    Some(TypeInfo::Character { len: None, .. })
                ) && !arg_sym.attrs.allocatable
                    && !is_bind_c;
                if is_assumed_len {
                    let reg = if reg_idx < 8 {
                        format!("x{}", reg_idx)
                    } else {
                        format!("stack+{}", (reg_idx - 8) * 8)
                    };
                    writeln!(out, "  @arg {}@len : integer(8)", arg_name).unwrap();
                    writeln!(out, "    @abi pass={} width=8 hidden", reg).unwrap();
                    reg_idx += 1;
                }
            }
        }
    }

    // @hint line.
    if let Some(func) = ir_func {
        let mut hints = Vec::new();
        if is_leaf(func) {
            hints.push("leaf".to_string());
        }
        if !touches_globals(func) {
            hints.push("no_globals".to_string());
        }
        let cost: usize = func.blocks.iter().map(|b| b.insts.len()).sum();
        hints.push(format!("cost={}", cost));
        writeln!(out, "  @hint {}", hints.join(" ")).unwrap();
    }

    writeln!(out, "@end {}", kind_str).unwrap();
    writeln!(out).unwrap();
}

fn is_leaf(func: &Function) -> bool {
    for block in &func.blocks {
        for inst in &block.insts {
            match &inst.kind {
                InstKind::Call(..) | InstKind::RuntimeCall(..) => return false,
                _ => {}
            }
        }
    }
    true
}

fn touches_globals(func: &Function) -> bool {
    for block in &func.blocks {
        for inst in &block.insts {
            match &inst.kind {
                InstKind::GlobalAddr(_) => return true,
                InstKind::Call(FuncRef::External(_), _) => return true,
                _ => {}
            }
        }
    }
    false
}

fn emit_type(out: &mut String, name: &str, type_layouts: &TypeLayoutRegistry) {
    writeln!(out, "@type {}", name).unwrap();
    if let Some(layout) = type_layouts.get(&name.to_lowercase()) {
        writeln!(out, "  @layout size={} align={}", layout.size, layout.align).unwrap();
        if let Some(ref parent) = layout.parent {
            writeln!(out, "  @extends {}", parent).unwrap();
        }
        if layout.is_abstract {
            writeln!(out, "  @abstract").unwrap();
        }
        for field in &layout.fields {
            let ft = type_info_to_string(Some(&field.type_info));
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
            if field.target {
                attrs.push_str(" @target");
            }
            if field.declared_array && field.dims.is_empty() {
                attrs.push_str(" @declared_array");
            }
            if let Some(default_init) = &field.default_init {
                attrs.push_str(&render_field_default_init(default_init));
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
            if bp.method_name == bp.target_name {
                if bp.nopass {
                    writeln!(out, "  @binds {}, nopass{}", bp.method_name, abi_suffix).unwrap();
                } else {
                    writeln!(out, "  @binds {}{}", bp.method_name, abi_suffix).unwrap();
                }
            } else {
                if bp.nopass {
                    writeln!(
                        out,
                        "  @binds {} => {}, nopass{}",
                        bp.method_name, bp.target_name, abi_suffix
                    )
                    .unwrap();
                } else {
                    writeln!(out, "  @binds {} => {}{}", bp.method_name, bp.target_name, abi_suffix)
                        .unwrap();
                }
            }
        }

        fn render_field_default_init(init: &crate::sema::type_layout::FieldDefaultInit) -> String {
            match init {
                crate::sema::type_layout::FieldDefaultInit::Character(value) => {
                    format!(" @init=charhex:{}", hex_encode_bytes(value.as_bytes()))
                }
                crate::sema::type_layout::FieldDefaultInit::Integer(value) => {
                    format!(" @init=int:{}", value)
                }
                crate::sema::type_layout::FieldDefaultInit::Logical(value) => {
                    format!(" @init=logical:{}", if *value { "true" } else { "false" })
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
            writeln!(out, "  @final {}", fp).unwrap();
        }
        if let Some(owner_module) = &layout.owner_module {
            writeln!(out, "  @owner {}", owner_module).unwrap();
        }
        writeln!(out, "  @tag {}", layout.type_tag).unwrap();
    }
    writeln!(out, "@end type").unwrap();
    writeln!(out).unwrap();
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
    writeln!(out, "@interface {}{}", name, suf).unwrap();
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

fn type_info_to_string(info: Option<&TypeInfo>) -> String {
    match info {
        Some(TypeInfo::Integer { kind }) => match kind {
            Some(k) => format!("integer({})", k),
            None => "integer".to_string(),
        },
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
        Some(TypeInfo::Character { len, kind: _ }) => match len {
            Some(n) => format!("character(len={})", n),
            None => "character(len=:)".to_string(),
        },
        Some(TypeInfo::Derived(name)) => format!("type({})", name),
        Some(TypeInfo::Class(name)) => format!("class({})", name),
        Some(TypeInfo::ClassStar) => "class(*)".to_string(),
        Some(TypeInfo::TypeStar) => "type(*)".to_string(),
        None => "unknown".to_string(),
    }
}

fn fnv1a_hex(content: &str) -> String {
    // FNV-1a 64-bit hash for source content fingerprinting.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in content.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
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
    /// Sprint35-SMP Phase 1: rank of the dummy (number of array dimensions);
    /// 0 for scalar. When non-zero the loader reconstructs a SymbolAttrs
    /// `array_spec` of this rank, deriving each dim's kind from the
    /// `descriptor` / `allocatable` / `pointer` flags. Bound expressions
    /// (Explicit lower:upper) are not preserved across .amod boundaries —
    /// SMP-body synthesis only needs the shape kind and rank for Phase 2.
    pub rank: u8,
}

/// A procedure parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct AmodProc {
    pub name: String,
    pub kind: SymbolKind,
    pub return_type: Option<TypeInfo>,
    pub result_allocatable: bool,
    pub result_pointer: bool,
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
    pub access: Access,
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
    pub ir_symbol: Option<String>,
    pub deferred_char: bool,
    pub dims: Vec<(i64, i64)>,
    pub const_value: Option<i64>,
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

/// One renamed USE association from this module's source: `use M, only: A => B`
/// becomes `UseRename { local: "a", original: "b", source_module: "m" }`. The
/// rename is recorded so downstream consumers (esp. submodules) can resolve
/// the local name at .amod-load time. Without this the kind constant
/// `block_kind => int64` is irrecoverable from a binary-only build.
#[derive(Debug, Clone)]
pub struct UseRename {
    pub local: String,
    pub original: String,
    pub source_module: String,
}

/// Complete module interface parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct ModuleInterface {
    pub module_name: String,
    pub dependencies: Vec<String>,
    pub renames: Vec<UseRename>,
    pub variables: Vec<AmodVar>,
    pub procedures: Vec<AmodProc>,
    pub types: Vec<crate::sema::type_layout::TypeLayout>,
    pub interfaces: Vec<AmodInterface>,
    pub checksum: Option<String>,
}

/// Read a `.amod` file and return the parsed module interface.
pub fn read_amod(path: &Path) -> Result<ModuleInterface, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    parse_amod(&content, path)
}

fn parse_amod(content: &str, path: &Path) -> Result<ModuleInterface, String> {
    let mut lines = content.lines().peekable();

    // Header: #!amod N
    let magic = lines.next().ok_or("empty .amod file")?;
    if !magic.starts_with("#!amod ") {
        return Err(format!(
            "{}: not an .amod file (missing #!amod magic)",
            path.display()
        ));
    }
    let version: u32 = magic[7..]
        .trim()
        .parse()
        .map_err(|_| format!("{}: invalid .amod version", path.display()))?;
    if version > 2 {
        eprintln!("warning: {}: .amod version {} is newer than this compiler supports; some information may be ignored", path.display(), version);
    }

    let mut module_name = String::new();
    let mut checksum = None;

    // Parse # key: value header lines.
    while let Some(line) = lines.peek() {
        if let Some(rest) = line.strip_prefix("# ") {
            if let Some((key, val)) = rest.split_once(": ") {
                match key {
                    "module" => module_name = val.trim().to_string(),
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

    let mut dependencies = Vec::new();
    let mut renames: Vec<UseRename> = Vec::new();
    let mut variables = Vec::new();
    let mut procedures = Vec::new();
    let mut types = Vec::new();
    let mut interfaces = Vec::new();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(dep) = trimmed.strip_prefix("@uses ") {
            dependencies.push(dep.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@use_rename ") {
            // `@use_rename <local> = <original> from <module>`
            if let Some((lhs, mod_part)) = rest.split_once(" from ") {
                if let Some((local, original)) = lhs.split_once(" = ") {
                    renames.push(UseRename {
                        local: local.trim().to_string(),
                        original: original.trim().to_string(),
                        source_module: mod_part.trim().to_string(),
                    });
                }
            }
        } else if trimmed.starts_with("@var ") {
            variables.push(parse_var(trimmed, false));
        } else if trimmed.starts_with("@param ") {
            variables.push(parse_var(&trimmed.replacen("@param", "@var", 1), true));
        } else if trimmed.starts_with("@function ") || trimmed.starts_with("@subroutine ") {
            let proc = parse_proc(trimmed, &mut lines);
            procedures.push(proc);
        } else if trimmed.starts_with("@type ") {
            let layout = parse_type(trimmed, &mut lines);
            types.push(layout);
        } else if let Some(name) = trimmed.strip_prefix("@interface ") {
            // Generic interface block: header is `@interface <name>[, private]`,
            // body lists `@specific <proc>` until `@end interface`.
            let header = name.trim();
            let (iface_name, access) = match header.split_once(", ") {
                Some((n, attr)) if attr.split(", ").any(|a| a == "private") => {
                    (n.trim().to_string(), Access::Private)
                }
                _ => (header.to_string(), Access::Public),
            };
            let mut specifics = Vec::new();
            for iline in lines.by_ref() {
                let t = iline.trim();
                if t.starts_with("@end interface") {
                    break;
                }
                if let Some(spec) = t.strip_prefix("@specific ") {
                    specifics.push(spec.trim().to_string());
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

    Ok(ModuleInterface {
        module_name,
        dependencies,
        renames,
        variables,
        procedures,
        types,
        interfaces,
        checksum,
    })
}

fn parse_var(line: &str, is_param: bool) -> AmodVar {
    // @var name : type[, attrs...] [@ir symbol] [@deferred_char] [@dims ...]
    let rest = line.strip_prefix("@var ").unwrap_or(line);
    let (name_type, ir_part) = if let Some(idx) = rest.find(" @ir ") {
        (&rest[..idx], Some(&rest[idx + 5..]))
    } else {
        (rest, None)
    };

    let (name, type_and_attrs) = name_type
        .split_once(" : ")
        .unwrap_or((name_type, "unknown"));
    let name = name.trim().to_string();

    // Split type from attrs on comma.
    let (type_str, attr_str) = if let Some(idx) = type_and_attrs.find(", ") {
        (&type_and_attrs[..idx], &type_and_attrs[idx + 2..])
    } else {
        (type_and_attrs, "")
    };

    let mut const_value = None;
    // For @param with `= value`, strip the value suffix from the
    // type string before parsing the type.
    let clean_type_str = if is_param {
        if let Some(eq_idx) = type_str.rfind(" = ") {
            let val_str = type_str[eq_idx + 3..].trim();
            if let Ok(v) = val_str.parse::<i64>() {
                const_value = Some(v);
            }
            &type_str[..eq_idx]
        } else {
            type_str
        }
    } else {
        type_str
    };

    let type_info = parse_type_info(clean_type_str.trim());
    let allocatable = attr_str.contains("allocatable");
    let save = attr_str.contains("save");
    let pointer = attr_str.contains("pointer");
    let proc_pointer = attr_str.contains("procptr");
    let target = attr_str.contains("target");
    let access = if attr_str.contains("private") {
        Access::Private
    } else {
        Access::Public
    };

    let mut ir_symbol = None;
    let mut deferred_char = false;
    let mut dims = Vec::new();

    if let Some(ir) = ir_part {
        let parts: Vec<&str> = ir.split_whitespace().collect();
        if !parts.is_empty() {
            ir_symbol = Some(parts[0].to_string());
        }
        let mut i = 1;
        while i < parts.len() {
            if parts[i] == "@deferred_char" {
                deferred_char = true;
                i += 1;
            } else if parts[i] == "@dims" {
                // Parse dimension pairs: @dims 1:5 1:10 ...
                i += 1;
                while i < parts.len() && parts[i].contains(':') && !parts[i].starts_with('@') {
                    let pair = parts[i];
                    if let Some((lo_s, ext_s)) = pair.split_once(':') {
                        let lo = lo_s.parse::<i64>().unwrap_or(1);
                        let ext = ext_s.parse::<i64>().unwrap_or(1);
                        dims.push((lo, ext));
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
    }

    AmodVar {
        name,
        type_info,
        is_parameter: is_param,
        allocatable,
        save,
        pointer,
        proc_pointer,
        target,
        ir_symbol,
        deferred_char,
        dims,
        const_value,
        access,
    }
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

fn parse_proc(header: &str, lines: &mut std::iter::Peekable<std::str::Lines>) -> AmodProc {
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
            Some(idx) => (
                rest[..idx].trim_end(),
                rest[idx + 1..].trim_start(),
            ),
            None => (rest.trim(), ""),
        }
    };

    let (name, return_type) = if let Some(arrow_idx) = name_and_ret.find(" -> ") {
        let n = &name_and_ret[..arrow_idx];
        let rt = parse_type_info(name_and_ret[arrow_idx + 4..].trim());
        (n.trim().to_string(), rt)
    } else {
        (name_and_ret.trim().to_string(), None)
    };

    let attr_chunks = split_attrs_top_level(attrs_str);
    let pure = attr_chunks.iter().any(|a| a == "pure");
    let elemental = attr_chunks.iter().any(|a| a == "elemental");
    let result_allocatable = attr_chunks.iter().any(|a| a == "result_allocatable");
    let result_pointer = attr_chunks.iter().any(|a| a == "result_pointer");
    let result_rank = attr_chunks
        .iter()
        .find_map(|attr| attr.strip_prefix("result_rank="))
        .and_then(|s| s.parse::<u8>().ok())
        .unwrap_or(0);
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

    let kind = if is_func {
        SymbolKind::Function
    } else {
        SymbolKind::Subroutine
    };

    let mut args = Vec::new();

    // Parse body lines until @end.
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.starts_with("@end ") {
            break;
        }
        if trimmed.starts_with("@arg ") {
            args.push(parse_arg(trimmed));
        }
        // Skip @abi and @hint lines (informational; reader uses
        // them for optimization but not for correctness).
    }

    let result_array_bounds = attr_chunks
        .iter()
        .find_map(|attr| attr.strip_prefix("result_array_bounds="))
        .map(|s| s.trim_matches('"').to_string());

    AmodProc {
        name,
        kind,
        return_type,
        result_allocatable,
        result_pointer,
        result_rank,
        result_name,
        result_array_bounds,
        pure,
        elemental,
        access,
        binding_label,
        args,
    }
}

fn parse_arg(line: &str) -> AmodArg {
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

    let type_info = parse_type_info(type_str.trim());
    let intent = if attr_str.contains("intent(in)") && !attr_str.contains("intent(inout)") {
        Some(Intent::In)
    } else if attr_str.contains("intent(out)") {
        Some(Intent::Out)
    } else if attr_str.contains("intent(inout)") {
        Some(Intent::InOut)
    } else {
        None
    };

    let optional = attr_str.contains("optional");
    let value = attr_str.contains("value");
    let descriptor = attr_str.contains("descriptor");
    let allocatable = attr_str.contains("allocatable");
    let pointer = attr_str.contains("pointer");
    let external = attr_str
        .split(", ")
        .any(|tok| tok.trim().eq_ignore_ascii_case("external"));
    // Sprint35-SMP Phase 1: parse `rank=N` if present. Emitted only when
    // the dummy is array-shaped; absence means rank 0 (scalar).
    let rank = attr_str
        .split(", ")
        .find_map(|tok| tok.strip_prefix("rank="))
        .and_then(|s| s.trim().parse::<u8>().ok())
        .unwrap_or(0);
    let procedure_iface = attr_str.split(", ").find_map(|tok| {
        let t = tok.trim();
        let inner = t.strip_prefix("procedure(")?;
        inner.strip_suffix(')').map(|s| s.trim().to_string())
    });

    AmodArg {
        name,
        type_info,
        intent,
        optional,
        value,
        descriptor,
        allocatable,
        pointer,
        hidden,
        external,
        procedure_iface,
        rank,
    }
}

fn parse_type(
    header: &str,
    lines: &mut std::iter::Peekable<std::str::Lines>,
) -> crate::sema::type_layout::TypeLayout {
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
        if let Some(value) = payload.strip_prefix("charhex:") {
            let decoded = String::from_utf8(hex_decode_bytes(value)?).ok()?;
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

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.starts_with("@end type") {
            break;
        }

        if let Some(rest) = trimmed.strip_prefix("@layout ") {
            for part in rest.split_whitespace() {
                if let Some(v) = part.strip_prefix("size=") {
                    size = v.parse().unwrap_or(0);
                } else if let Some(v) = part.strip_prefix("align=") {
                    align = v.parse().unwrap_or(1);
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("@extends ") {
            parent = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@field ") {
            // @field name : type @offset N @size M [@allocatable] [@pointer] [@target]
            if let Some((name_type, offset_part)) = rest.split_once(" @offset ") {
                let (fname, ftype_str) = name_type
                    .split_once(" : ")
                    .unwrap_or((name_type, "unknown"));
                // Split off the size and any trailing attribute flags.
                let (offset_str, after_offset) = match offset_part.find(" @size ") {
                    Some(idx) => (&offset_part[..idx], &offset_part[idx + 7..]),
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
                    for dim in dims_str
                        .split(',')
                        .map(str::trim)
                        .filter(|dim| !dim.is_empty())
                    {
                        if let Some((lower_str, upper_str)) = dim.split_once(':') {
                            let lower = lower_str.trim().parse().unwrap_or(1);
                            let upper = upper_str.trim().parse().unwrap_or(lower - 1);
                            dims.push((lower, (upper - lower + 1).max(0)));
                        }
                    }
                    flag_tail = tail;
                } else if let Some(idx) = after_offset.find(" @") {
                    size_str = &after_offset[..idx];
                    flag_tail = &after_offset[idx..];
                }
                let mut allocatable = false;
                let mut pointer = false;
                let mut target = false;
                let mut declared_array = false;
                let mut default_init = None;
                for token in flag_tail.split_whitespace() {
                    match token {
                        "@allocatable" => allocatable = true,
                        "@pointer" => pointer = true,
                        "@target" => target = true,
                        "@declared_array" => declared_array = true,
                        _ => {
                            if let Some(init) = parse_field_default_init_token(token) {
                                default_init = Some(init);
                            }
                        }
                    }
                }
                declared_array |= !dims.is_empty();
                let ftype = parse_type_info(ftype_str.trim());
                fields.push(FieldLayout {
                    name: fname.trim().to_string(),
                    offset: offset_str.trim().parse().unwrap_or(0),
                    size: size_str.trim().parse().unwrap_or(0),
                    dims,
                    declared_array,
                    type_info: ftype.unwrap_or(TypeInfo::Integer { kind: None }),
                    allocatable,
                    pointer,
                    target,
                    default_init,
                });
            }
        } else if let Some(rest) = trimmed.strip_prefix("@binds ") {
            let (clean, abi_name) = if let Some((bind_part, abi_part)) = rest.split_once(" @abi ") {
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
            });
        } else if let Some(rest) = trimmed.strip_prefix("@final ") {
            final_procs.push(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@owner ") {
            owner_module = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@tag ") {
            type_tag = rest.trim().parse().unwrap_or(0);
        } else if trimmed == "@abstract" {
            is_abstract = true;
        }
    }

    TypeLayout {
        name,
        owner_module,
        size,
        align,
        fields,
        bound_procs,
        final_procs,
        type_tag,
        parent,
        is_abstract,
    }
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
        // character(len=N) or character(len=:)
        if let Some(inner) = s
            .strip_prefix("character(len=")
            .and_then(|r| r.strip_suffix(')'))
        {
            if inner == ":" {
                return Some(TypeInfo::Character {
                    len: None,
                    kind: None,
                });
            } else if let Ok(n) = inner.parse::<i64>() {
                return Some(TypeInfo::Character {
                    len: Some(n),
                    kind: None,
                });
            }
        }
        return Some(TypeInfo::Character {
            len: None,
            kind: None,
        });
    }
    if let Some(inner) = s.strip_prefix("type(").and_then(|r| r.strip_suffix(')')) {
        return Some(TypeInfo::Derived(inner.to_string()));
    }
    if let Some(inner) = s.strip_prefix("class(").and_then(|r| r.strip_suffix(')')) {
        return Some(TypeInfo::Class(inner.to_string()));
    }

    None
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
        if var.is_parameter && var.ir_symbol.is_none() {
            continue;
        } // PARAMETERs with folded values inline; others still need storage
        if let Some(ref ir_sym) = var.ir_symbol {
            let derived_type = match var.type_info.as_ref() {
                Some(TypeInfo::Derived(name))
                    if !matches!(name.to_lowercase().as_str(), "c_ptr" | "c_funptr") =>
                {
                    Some(name.clone())
                }
                _ => None,
            };
            let ir_ty = if var.proc_pointer {
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
                    symbol: ir_sym.clone(),
                    ty: ir_ty,
                    dims: var.dims.clone(),
                    allocatable: var.allocatable,
                    is_pointer: var.pointer,
                    deferred_char: var.deferred_char,
                    derived_type,
                    char_kind: match var.type_info.as_ref() {
                        Some(crate::sema::symtab::TypeInfo::Character { len: Some(n), .. }) => {
                            crate::ir::lower::CharKind::Fixed(*n)
                        }
                        _ if var.deferred_char => crate::ir::lower::CharKind::Deferred,
                        _ => crate::ir::lower::CharKind::None,
                    },
                    external: true,
                    private: var.access == Access::Private,
                },
            );
        }
    }
    out
}

/// Extract char_len_star_params from a loaded ModuleInterface.
/// For each procedure with character(len=*) args, produces a
/// Vec<bool> (per-position, true = assumed-length character).
pub fn extract_optional_params(iface: &ModuleInterface) -> HashMap<String, Vec<bool>> {
    let mut out = HashMap::new();
    for proc in &iface.procedures {
        let visible_args: Vec<&AmodArg> = proc.args.iter().filter(|a| !a.hidden).collect();
        let flags: Vec<bool> = visible_args.iter().map(|a| a.optional).collect();
        if !flags.is_empty() {
            let key = proc.name.to_lowercase();
            out.insert(key.clone(), flags.clone());
            out.insert(
                format!(
                    "afs_modproc_{}_{}",
                    iface.module_name.to_lowercase(),
                    key
                ),
                flags,
            );
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
        let is_bind_c = proc.binding_label.is_some();
        let visible_args: Vec<&AmodArg> = proc.args.iter().filter(|a| !a.hidden).collect();
        let flags: Vec<bool> = visible_args
            .iter()
            .map(|a| {
                matches!(a.type_info, Some(TypeInfo::Character { len: None, .. }))
                    && !a.allocatable
                    && !is_bind_c
            })
            .collect();
        if !flags.is_empty() {
            let key = proc.name.to_lowercase();
            out.insert(key.clone(), flags.clone());
            out.insert(
                format!(
                    "afs_modproc_{}_{}",
                    iface.module_name.to_lowercase(),
                    key
                ),
                flags,
            );
        }
    }
    out
}

/// Extract descriptor_params from a loaded ModuleInterface.
/// For each procedure with descriptor-backed dummies, produces a
/// Vec<bool> (per-position, true = pass the 384-byte descriptor).
pub fn extract_descriptor_params(iface: &ModuleInterface) -> HashMap<String, Vec<bool>> {
    let mut out = HashMap::new();
    for proc in &iface.procedures {
        let visible_args: Vec<&AmodArg> = proc.args.iter().filter(|a| !a.hidden).collect();
        let flags: Vec<bool> = visible_args.iter().map(|a| a.descriptor).collect();
        if !flags.is_empty() {
            let key = proc.name.to_lowercase();
            out.insert(key.clone(), flags.clone());
            out.insert(
                format!(
                    "afs_modproc_{}_{}",
                    iface.module_name.to_lowercase(),
                    key
                ),
                flags,
            );
        }
    }
    out
}

fn type_info_to_ir_type(info: Option<&TypeInfo>) -> crate::ir::types::IrType {
    use crate::ir::types::{FloatWidth, IntWidth, IrType};
    match info {
        Some(TypeInfo::Derived(name)) => {
            let lower = name.to_lowercase();
            if lower == "c_ptr" || lower == "c_funptr" {
                IrType::Int(IntWidth::I64)
            } else {
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))
            }
        }
        Some(TypeInfo::Integer { kind }) => IrType::Int(match kind {
            Some(1) => IntWidth::I8,
            Some(2) => IntWidth::I16,
            Some(8) => IntWidth::I64,
            Some(16) => IntWidth::I128,
            _ => IntWidth::I32,
        }),
        Some(TypeInfo::Real { kind }) => IrType::Float(match kind {
            Some(8) => FloatWidth::F64,
            _ => FloatWidth::F32,
        }),
        Some(TypeInfo::DoublePrecision) => IrType::Float(FloatWidth::F64),
        Some(TypeInfo::Complex { kind }) => {
            let fw = match kind {
                Some(8) => FloatWidth::F64,
                _ => FloatWidth::F32,
            };
            IrType::Array(Box::new(IrType::Float(fw)), 2)
        }
        Some(TypeInfo::Logical { .. }) => IrType::Bool,
        Some(TypeInfo::Character { .. }) => IrType::Int(IntWidth::I8),
        _ => IrType::Int(IntWidth::I32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
  @tag 1
@end type
"#;
        let iface = parse_amod(amod_text, Path::new("test.amod")).unwrap();
        assert_eq!(iface.module_name, "physics");
        assert_eq!(iface.dependencies, vec!["iso_c_binding"]);
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
        let pt = &iface.types[0];
        assert_eq!(pt.name, "particle");
        assert_eq!(pt.size, 12);
        assert_eq!(pt.fields.len(), 3);
        assert_eq!(pt.bound_procs.len(), 1);
        assert_eq!(pt.bound_procs[0].method_name, "kinetic_energy");
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
    }
}
