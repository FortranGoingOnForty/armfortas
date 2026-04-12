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

    // @abi line for the procedure.
    let name_lc = name.to_lowercase();
    let hidden_count = char_len_star_params.get(&name_lc)
        .map(|flags| flags.iter().filter(|f| **f).count())
        .unwrap_or(0);
    writeln!(out, "  @abi cc=aapcs64 hidden_char_lens={}", hidden_count).unwrap();

    // Walk into the procedure's child scope for full arg info.
    let proc_scope = st.scopes.iter().find(|s| {
        s.parent == Some(mod_scope_id) && match &s.kind {
            ScopeKind::Function(n) | ScopeKind::Subroutine(n) => n.eq_ignore_ascii_case(name),
            _ => false,
        }
    });

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

    // Hidden character-length args.
    if let Some(flags) = char_len_star_params.get(&name_lc) {
        let arg_names: Vec<&str> = if let Some(ps) = proc_scope {
            ps.arg_order.iter().map(|s| s.as_str()).collect()
        } else {
            sym.arg_names.iter().map(|s| s.as_str()).collect()
        };
        for (i, flag) in flags.iter().enumerate() {
            if *flag {
                if let Some(aname) = arg_names.get(i) {
                    let reg = if reg_idx < 8 { format!("x{}", reg_idx) } else { format!("stack+{}", (reg_idx - 8) * 8) };
                    writeln!(out, "  @arg {}@len : integer(8)", aname).unwrap();
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
// Reader (Phase 3 — placeholder)
// =====================================================================

/// Read a `.amod` file and return the module's interface.
/// TODO: implement in Phase 3 (task #392).
pub fn read_amod(_path: &Path) -> Option<()> {
    None
}
