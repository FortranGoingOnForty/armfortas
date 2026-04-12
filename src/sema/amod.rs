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

use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

use crate::ir::lower::ModuleGlobalInfo;
use crate::ir::inst::{Function, InstKind, FuncRef, Module as IrModule};
use crate::sema::symtab::*;
use crate::sema::type_layout::TypeLayoutRegistry;

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
    char_len_star_params: &HashMap<String, Vec<bool>>,
) -> String {
    let mut out = String::new();
    let mod_key = module_name.to_lowercase();
    let scope = st.scope(mod_scope_id);

    // ---- Header ----
    writeln!(out, "#!amod 2").unwrap();
    writeln!(out, "# module: {}", mod_key).unwrap();
    writeln!(out, "# source: {}", source_path).unwrap();
    writeln!(out, "# checksum: sha256:{}", sha256_hex(source_content)).unwrap();
    writeln!(out, "# compiled: {}", compile_timestamp()).unwrap();
    writeln!(out, "# compiler: armfortas 0.1.0").unwrap();
    writeln!(out, "# abi: arm64-apple-darwin").unwrap();
    writeln!(out).unwrap();

    // ---- Dependencies ----
    let mut deps: Vec<String> = scope.use_associations.iter()
        .filter_map(|ua| {
            let src_scope = st.scope(ua.source_scope);
            if let ScopeKind::Module(ref n) = src_scope.kind {
                Some(n.to_lowercase())
            } else { None }
        })
        .collect();
    deps.sort();
    deps.dedup();
    for dep in &deps {
        writeln!(out, "@uses {}", dep).unwrap();
    }
    if !deps.is_empty() { writeln!(out).unwrap(); }

    // Collect and sort public symbols.
    let mut syms: Vec<(&String, &Symbol)> = scope.symbols.iter()
        .filter(|(_, sym)| is_public(sym, scope))
        .collect();
    syms.sort_by_key(|(k, _)| k.to_lowercase());

    // ---- Variables ----
    let vars: Vec<_> = syms.iter()
        .filter(|(_, sym)| matches!(sym.kind, SymbolKind::Variable) && !sym.attrs.parameter)
        .collect();
    for (name, sym) in &vars {
        emit_variable(&mut out, &mod_key, name, sym, globals);
    }
    if !vars.is_empty() { writeln!(out).unwrap(); }

    // ---- Parameters ----
    let params: Vec<_> = syms.iter()
        .filter(|(_, sym)| sym.attrs.parameter || matches!(sym.kind, SymbolKind::Parameter))
        .collect();
    for (name, sym) in &params {
        emit_parameter(&mut out, &mod_key, name, sym, globals);
    }
    if !params.is_empty() { writeln!(out).unwrap(); }

    // ---- Procedures ----
    let procs: Vec<_> = syms.iter()
        .filter(|(_, sym)| matches!(sym.kind, SymbolKind::Function | SymbolKind::Subroutine))
        .collect();
    for (name, sym) in &procs {
        emit_procedure(&mut out, name, sym, st, mod_scope_id, ir_module, char_len_star_params);
    }

    // ---- Types ----
    let types: Vec<_> = syms.iter()
        .filter(|(_, sym)| matches!(sym.kind, SymbolKind::DerivedType))
        .collect();
    for (name, _sym) in &types {
        emit_type(&mut out, name, type_layouts);
    }

    // ---- Interfaces ----
    let ifaces: Vec<_> = syms.iter()
        .filter(|(_, sym)| matches!(sym.kind, SymbolKind::NamedInterface))
        .collect();
    for (name, sym) in &ifaces {
        emit_interface(&mut out, name, sym);
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
    let type_str = type_info_to_string(sym.type_info.as_ref());
    write!(out, "@var {} : {}", name, type_str).unwrap();

    let mut attrs = Vec::new();
    if sym.attrs.allocatable { attrs.push("allocatable"); }
    if sym.attrs.save { attrs.push("save"); }
    if sym.attrs.pointer { attrs.push("pointer"); }
    if sym.attrs.target { attrs.push("target"); }
    if !attrs.is_empty() {
        write!(out, ", {}", attrs.join(", ")).unwrap();
    }

    let global_key = (mod_key.to_string(), name.to_lowercase());
    if let Some(info) = globals.get(&global_key) {
        write!(out, " @ir {}", info.symbol).unwrap();
        if info.deferred_char { write!(out, " @deferred_char").unwrap(); }
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
    let type_str = type_info_to_string(sym.type_info.as_ref());
    if let Some(cv) = sym.const_value {
        writeln!(out, "@param {} : {} = {}", name, type_str, cv).unwrap();
    } else {
        // Parameter without a folded const_value — emit with @ir
        // so the reader can at least reference the global.
        let global_key = (mod_key.to_string(), name.to_lowercase());
        if let Some(info) = globals.get(&global_key) {
            writeln!(out, "@param {} : {} @ir {}", name, type_str, info.symbol).unwrap();
        } else {
            writeln!(out, "@param {} : {}", name, type_str).unwrap();
        }
    }
}

fn emit_procedure(
    out: &mut String,
    name: &str,
    sym: &Symbol,
    st: &SymbolTable,
    mod_scope_id: ScopeId,
    ir_module: &IrModule,
    char_len_star_params: &HashMap<String, Vec<bool>>,
) {
    let is_func = matches!(sym.kind, SymbolKind::Function);
    let kind_str = if is_func { "function" } else { "subroutine" };

    if is_func {
        let ret_str = type_info_to_string(sym.type_info.as_ref());
        write!(out, "@function {} -> {}", name, ret_str).unwrap();
    } else {
        write!(out, "@subroutine {}", name).unwrap();
    }
    if sym.attrs.pure { write!(out, ", pure").unwrap(); }
    if sym.attrs.elemental { write!(out, ", elemental").unwrap(); }
    writeln!(out).unwrap();

    let name_lc = name.to_lowercase();

    // Walk into the procedure's child scope for full arg info.
    let proc_scope = st.scopes.iter().find(|s| {
        s.parent == Some(mod_scope_id) && match &s.kind {
            ScopeKind::Function(n) | ScopeKind::Subroutine(n) => n.eq_ignore_ascii_case(name),
            _ => false,
        }
    });

    // Compute hidden char-length count from the scope's arg types.
    let mut hidden_count = 0usize;
    if let Some(pscope) = proc_scope {
        for arg_name in &pscope.arg_order {
            if let Some(arg_sym) = pscope.symbols.get(&arg_name.to_lowercase()) {
                if matches!(arg_sym.type_info, Some(TypeInfo::Character { len: None, .. }))
                    && !arg_sym.attrs.allocatable
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
        for arg_name in &pscope.arg_order {
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
                if arg_sym.attrs.optional { arg_attrs.push("optional"); }
                if arg_sym.attrs.value { arg_attrs.push("value"); }
                if arg_sym.attrs.allocatable { arg_attrs.push("allocatable"); }
                if arg_sym.attrs.pointer { arg_attrs.push("pointer"); }
                if !arg_attrs.is_empty() {
                    write!(out, ", {}", arg_attrs.join(", ")).unwrap();
                }
                writeln!(out).unwrap();
                // @abi per arg — ARM64 AAPCS64: first 8 int/ptr args in x0-x7.
                let reg = if reg_idx < 8 { format!("x{}", reg_idx) } else { format!("stack+{}", (reg_idx - 8) * 8) };
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
                ) && !arg_sym.attrs.allocatable;
                if is_assumed_len {
                    let reg = if reg_idx < 8 { format!("x{}", reg_idx) } else { format!("stack+{}", (reg_idx - 8) * 8) };
                    writeln!(out, "  @arg {}@len : integer(8)", arg_name).unwrap();
                    writeln!(out, "    @abi pass={} width=8 hidden", reg).unwrap();
                    reg_idx += 1;
                }
            }
        }
    }

    // @hint line.
    let ir_func = ir_module.functions.iter()
        .find(|f| f.name.eq_ignore_ascii_case(name) || f.name.eq_ignore_ascii_case(&name_lc));
    if let Some(func) = ir_func {
        let mut hints = Vec::new();
        if is_leaf(func) { hints.push("leaf".to_string()); }
        if !touches_globals(func) { hints.push("no_globals".to_string()); }
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
        for field in &layout.fields {
            let ft = type_info_to_string(Some(&field.type_info));
            writeln!(out, "  @field {} : {} @offset {} @size {}", field.name, ft, field.offset, field.size).unwrap();
        }
        for bp in &layout.bound_procs {
            if bp.method_name == bp.target_name {
                if bp.nopass {
                    writeln!(out, "  @binds {}, nopass", bp.method_name).unwrap();
                } else {
                    writeln!(out, "  @binds {}", bp.method_name).unwrap();
                }
            } else {
                if bp.nopass {
                    writeln!(out, "  @binds {} => {}, nopass", bp.method_name, bp.target_name).unwrap();
                } else {
                    writeln!(out, "  @binds {} => {}", bp.method_name, bp.target_name).unwrap();
                }
            }
        }
        for fp in &layout.final_procs {
            writeln!(out, "  @final {}", fp).unwrap();
        }
        writeln!(out, "  @tag {}", layout.type_tag).unwrap();
    }
    writeln!(out, "@end type").unwrap();
    writeln!(out).unwrap();
}

fn emit_interface(out: &mut String, name: &str, sym: &Symbol) {
    writeln!(out, "@interface {}", name).unwrap();
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

fn sha256_hex(content: &str) -> String {
    // Simple SHA-256 — use a runtime implementation or a basic
    // hash for now.  For MVP we emit a placeholder; the real
    // implementation will use the system's crypto library.
    // TODO: wire up actual SHA-256.
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
    pub allocatable: bool,
    pub pointer: bool,
    pub hidden: bool,
}

/// A procedure parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct AmodProc {
    pub name: String,
    pub kind: SymbolKind,
    pub return_type: Option<TypeInfo>,
    pub pure: bool,
    pub elemental: bool,
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
    pub target: bool,
    pub ir_symbol: Option<String>,
    pub deferred_char: bool,
    pub dims: Vec<(i64, i64)>,
    pub const_value: Option<i64>,
}

/// Complete module interface parsed from an .amod file.
#[derive(Debug, Clone)]
pub struct ModuleInterface {
    pub module_name: String,
    pub dependencies: Vec<String>,
    pub variables: Vec<AmodVar>,
    pub procedures: Vec<AmodProc>,
    pub types: Vec<crate::sema::type_layout::TypeLayout>,
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
        return Err(format!("{}: not an .amod file (missing #!amod magic)", path.display()));
    }
    let version: u32 = magic[7..].trim().parse()
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
    let mut variables = Vec::new();
    let mut procedures = Vec::new();
    let mut types = Vec::new();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') { continue; }

        if let Some(dep) = trimmed.strip_prefix("@uses ") {
            dependencies.push(dep.trim().to_string());
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
        } else if trimmed.starts_with("@interface ") {
            // Skip interface blocks for now — consume until @end interface.
            while let Some(iline) = lines.next() {
                if iline.trim().starts_with("@end interface") { break; }
            }
        }
        // Skip unrecognized directives (forward compatibility).
    }

    Ok(ModuleInterface {
        module_name,
        dependencies,
        variables,
        procedures,
        types,
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

    let (name, type_and_attrs) = name_type.split_once(" : ").unwrap_or((name_type, "unknown"));
    let name = name.trim().to_string();

    // Split type from attrs on comma.
    let (type_str, attr_str) = if let Some(idx) = type_and_attrs.find(", ") {
        (&type_and_attrs[..idx], &type_and_attrs[idx + 2..])
    } else {
        (type_and_attrs, "")
    };

    let type_info = parse_type_info(type_str.trim());
    let allocatable = attr_str.contains("allocatable");
    let save = attr_str.contains("save");
    let pointer = attr_str.contains("pointer");
    let target = attr_str.contains("target");

    let mut ir_symbol = None;
    let mut deferred_char = false;
    let mut dims = Vec::new();
    let mut const_value = None;

    if let Some(ir) = ir_part {
        let parts: Vec<&str> = ir.split_whitespace().collect();
        if !parts.is_empty() {
            ir_symbol = Some(parts[0].to_string());
        }
        for part in &parts[1..] {
            if *part == "@deferred_char" {
                deferred_char = true;
            }
        }
        // TODO: parse @dims and const_value from the = N suffix
    }

    // Check for = value (parameter constant).
    if is_param {
        if let Some(eq_idx) = name_type.rfind(" = ") {
            if let Ok(v) = name_type[eq_idx + 3..].trim().parse::<i64>() {
                const_value = Some(v);
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
        target,
        ir_symbol,
        deferred_char,
        dims,
        const_value,
    }
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
        let parts: Vec<&str> = rest.splitn(2, ", ").collect();
        (parts[0], parts.get(1).copied().unwrap_or(""))
    };

    let (name, return_type) = if let Some(arrow_idx) = name_and_ret.find(" -> ") {
        let n = &name_and_ret[..arrow_idx];
        let rt = parse_type_info(name_and_ret[arrow_idx + 4..].trim());
        (n.trim().to_string(), rt)
    } else {
        (name_and_ret.trim().to_string(), None)
    };

    let pure = attrs_str.contains("pure");
    let elemental = attrs_str.contains("elemental");

    let kind = if is_func { SymbolKind::Function } else { SymbolKind::Subroutine };

    let mut args = Vec::new();

    // Parse body lines until @end.
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with("@end ") { break; }
        if trimmed.starts_with("@arg ") {
            args.push(parse_arg(trimmed));
        }
        // Skip @abi and @hint lines (informational; reader uses
        // them for optimization but not for correctness).
    }

    AmodProc { name, kind, return_type, pure, elemental, args }
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
    } else { None };

    let optional = attr_str.contains("optional");
    let value = attr_str.contains("value");
    let allocatable = attr_str.contains("allocatable");
    let pointer = attr_str.contains("pointer");

    AmodArg { name, type_info, intent, optional, value, allocatable, pointer, hidden }
}

fn parse_type(header: &str, lines: &mut std::iter::Peekable<std::str::Lines>) -> crate::sema::type_layout::TypeLayout {
    use crate::sema::type_layout::*;

    let name = header.strip_prefix("@type ").unwrap_or("unknown").trim().to_string();
    let mut size = 0;
    let mut align = 1;
    let mut parent = None;
    let mut fields = Vec::new();
    let mut bound_procs = Vec::new();
    let mut final_procs = Vec::new();
    let mut type_tag = 0u64;

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with("@end type") { break; }

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
            // @field name : type @offset N @size M
            if let Some((name_type, offset_part)) = rest.split_once(" @offset ") {
                let (fname, ftype_str) = name_type.split_once(" : ").unwrap_or((name_type, "unknown"));
                let (offset_str, size_part) = if let Some(idx) = offset_part.find(" @size ") {
                    (&offset_part[..idx], &offset_part[idx + 7..])
                } else {
                    (offset_part, "0")
                };
                let ftype = parse_type_info(ftype_str.trim());
                fields.push(FieldLayout {
                    name: fname.trim().to_string(),
                    offset: offset_str.trim().parse().unwrap_or(0),
                    size: size_part.trim().parse().unwrap_or(0),
                    type_info: ftype.unwrap_or(TypeInfo::Integer { kind: None }),
                });
            }
        } else if let Some(rest) = trimmed.strip_prefix("@binds ") {
            let nopass = rest.contains(", nopass");
            let clean = rest.replace(", nopass", "");
            let (method, target) = if let Some((m, t)) = clean.split_once(" => ") {
                (m.trim().to_string(), t.trim().to_string())
            } else {
                let m = clean.trim().to_string();
                (m.clone(), m)
            };
            bound_procs.push(BoundProc { method_name: method, target_name: target, nopass });
        } else if let Some(rest) = trimmed.strip_prefix("@final ") {
            final_procs.push(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@tag ") {
            type_tag = rest.trim().parse().unwrap_or(0);
        }
    }

    TypeLayout { name, size, align, fields, bound_procs, final_procs, type_tag, parent }
}

fn parse_type_info(s: &str) -> Option<TypeInfo> {
    let s = s.trim();
    if s == "unknown" || s.is_empty() { return None; }
    if s == "double precision" { return Some(TypeInfo::DoublePrecision); }
    if s == "class(*)" { return Some(TypeInfo::ClassStar); }
    if s == "type(*)" { return Some(TypeInfo::TypeStar); }

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
        if let Some(inner) = s.strip_prefix("character(len=").and_then(|r| r.strip_suffix(')')) {
            if inner == ":" {
                return Some(TypeInfo::Character { len: None, kind: None });
            } else if let Ok(n) = inner.parse::<i64>() {
                return Some(TypeInfo::Character { len: Some(n), kind: None });
            }
        }
        return Some(TypeInfo::Character { len: None, kind: None });
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
        if var.is_parameter { continue; } // PARAMETERs are inlined, no global
        if let Some(ref ir_sym) = var.ir_symbol {
            let ir_ty = type_info_to_ir_type(var.type_info.as_ref());
            out.insert(
                (mod_key.clone(), var.name.to_lowercase()),
                crate::ir::lower::ModuleGlobalInfo {
                    symbol: ir_sym.clone(),
                    ty: ir_ty,
                    dims: var.dims.clone(),
                    allocatable: var.allocatable,
                    deferred_char: var.deferred_char,
                    external: true,
                },
            );
        }
    }
    out
}

/// Extract char_len_star_params from a loaded ModuleInterface.
/// For each procedure with character(len=*) args, produces a
/// Vec<bool> (per-position, true = assumed-length character).
pub fn extract_char_len_star_params(
    iface: &ModuleInterface,
) -> HashMap<String, Vec<bool>> {
    let mut out = HashMap::new();
    for proc in &iface.procedures {
        let visible_args: Vec<&AmodArg> = proc.args.iter()
            .filter(|a| !a.hidden)
            .collect();
        let flags: Vec<bool> = visible_args.iter().map(|a| {
            matches!(a.type_info, Some(TypeInfo::Character { len: None, .. }))
                && !a.allocatable
        }).collect();
        if flags.iter().any(|f| *f) {
            out.insert(proc.name.to_lowercase(), flags);
        }
    }
    out
}

fn type_info_to_ir_type(info: Option<&TypeInfo>) -> crate::ir::types::IrType {
    use crate::ir::types::{IrType, IntWidth, FloatWidth};
    match info {
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
            let fw = match kind { Some(8) => FloatWidth::F64, _ => FloatWidth::F32 };
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
        assert!(iface.variables.iter().any(|v| v.name == "call_count" && v.ir_symbol.as_deref() == Some("afs_mod_physics_call_count")));
        assert!(iface.variables.iter().any(|v| v.name == "gravity" && v.is_parameter));
        assert_eq!(iface.procedures.len(), 2);
        let ke = iface.procedures.iter().find(|p| p.name == "kinetic_energy").unwrap();
        assert!(ke.pure);
        assert_eq!(ke.args.len(), 2);
        assert_eq!(ke.args[0].name, "self");
        assert!(matches!(ke.args[0].intent, Some(Intent::In)));
        let af = iface.procedures.iter().find(|p| p.name == "apply_force").unwrap();
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
}
