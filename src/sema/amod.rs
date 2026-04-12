//! ARMFORTAS module file (.amod) writer and reader.
//!
//! When compiling with `-c`, the driver calls `write_amod` for each
//! MODULE program unit.  The resulting `.amod` file captures the
//! module's public interface — variables, procedures, derived types,
//! parameters — in a human-inspectable text format.
//!
//! When another compilation unit USEs a module not defined in-file,
//! the resolve phase calls `read_amod` to load the interface from
//! the file system, creating a synthetic module scope in the symbol
//! table and injecting `ModuleGlobalInfo` entries for the lowering
//! pass.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::Path;

use crate::ir::lower::ModuleGlobalInfo;
use crate::ir::types::IrType;
use crate::sema::symtab::*;
use crate::sema::type_layout::TypeLayoutRegistry;

/// Serialize a module's public interface to `.amod` text.
///
/// `mod_scope_id` identifies the module's scope in `st`.
/// `globals` maps (module_name, var_name) → ModuleGlobalInfo for
/// the IR-level symbol names.
/// `type_layouts` provides derived-type field info.
pub fn write_amod(
    module_name: &str,
    source_path: &str,
    st: &SymbolTable,
    mod_scope_id: ScopeId,
    globals: &HashMap<(String, String), ModuleGlobalInfo>,
    type_layouts: &TypeLayoutRegistry,
) -> String {
    let mut out = String::new();
    let mod_key = module_name.to_lowercase();

    writeln!(out, "# ARMFORTAS Module File").unwrap();
    writeln!(out, "@version 1").unwrap();
    writeln!(out, "@module {}", mod_key).unwrap();
    writeln!(out, "@source {}", source_path).unwrap();
    writeln!(out).unwrap();

    let scope = st.scope(mod_scope_id);

    // Sort symbols for deterministic output.
    let mut syms: Vec<(&String, &Symbol)> = scope.symbols.iter().collect();
    syms.sort_by_key(|(k, _)| k.clone());

    for (name, sym) in &syms {
        let access = match sym.attrs.access {
            Access::Private => continue, // skip private symbols
            Access::Public => "public",
            Access::Default => {
                match scope.default_access {
                    Access::Private => continue,
                    _ => "public",
                }
            }
        };

        match &sym.kind {
            SymbolKind::Variable | SymbolKind::Parameter => {
                let global_key = (mod_key.clone(), name.to_lowercase());
                let type_str = type_info_to_string(sym.type_info.as_ref());
                write!(out, "@variable {} : {}", name, type_str).unwrap();

                // Attributes.
                let mut attrs = Vec::new();
                if sym.attrs.allocatable { attrs.push("allocatable"); }
                if sym.attrs.save { attrs.push("save"); }
                if sym.attrs.parameter { attrs.push("parameter"); }
                if sym.attrs.pointer { attrs.push("pointer"); }
                if sym.attrs.target { attrs.push("target"); }
                if !attrs.is_empty() {
                    write!(out, ", {}", attrs.join(", ")).unwrap();
                }

                // Global symbol name (for IR reference).
                if let Some(info) = globals.get(&global_key) {
                    write!(out, " @symbol {}", info.symbol).unwrap();
                    if info.allocatable { write!(out, " @allocatable").unwrap(); }
                    if info.deferred_char { write!(out, " @deferred_char").unwrap(); }
                    if !info.dims.is_empty() {
                        write!(out, " @dims").unwrap();
                        for (lo, ext) in &info.dims {
                            write!(out, " {}:{}", lo, ext).unwrap();
                        }
                    }
                }

                // Constant value for PARAMETERs.
                if let Some(cv) = sym.const_value {
                    write!(out, " @const_value {}", cv).unwrap();
                }

                writeln!(out).unwrap();
            }

            SymbolKind::Function | SymbolKind::Subroutine => {
                let kind_str = if matches!(sym.kind, SymbolKind::Function) {
                    "function"
                } else {
                    "subroutine"
                };
                let ret_str = if matches!(sym.kind, SymbolKind::Function) {
                    type_info_to_string(sym.type_info.as_ref())
                } else {
                    "void".to_string()
                };
                write!(out, "@{} {} -> {}", kind_str, name, ret_str).unwrap();
                if sym.attrs.pure { write!(out, " pure").unwrap(); }
                if sym.attrs.elemental { write!(out, " elemental").unwrap(); }
                writeln!(out).unwrap();

                // Dummy argument names (we store names but not full
                // interface yet — a future version will add types
                // per arg from the scope's arg declarations).
                for arg in &sym.arg_names {
                    writeln!(out, "  @arg {}", arg).unwrap();
                }
                writeln!(out, "@end{}", kind_str).unwrap();
            }

            SymbolKind::DerivedType => {
                writeln!(out, "@type {}", name).unwrap();
                if let Some(layout) = type_layouts.get(&name.to_lowercase()) {
                    writeln!(out, "  @size {}", layout.size).unwrap();
                    for field in &layout.fields {
                        let ft = type_info_to_string(Some(&field.type_info));
                        writeln!(out, "  @field {} : {} @offset {}", field.name, ft, field.offset).unwrap();
                    }
                }
                writeln!(out, "@endtype").unwrap();
            }

            _ => {} // skip other symbol kinds
        }

        let _ = access; // suppress unused warning
    }

    out
}

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

// ---- Reader (Phase 3 — placeholder) ----

/// Read a `.amod` file and return the module's interface.
/// TODO: implement in Phase 3.
pub fn read_amod(_path: &Path) -> Option<()> {
    None
}
