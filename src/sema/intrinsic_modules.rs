//! Built-in intrinsic modules (iso_c_binding, iso_fortran_env).
//!
//! These modules are constructed programmatically rather than parsed
//! from source. When `USE iso_c_binding` is encountered, the symbol
//! table is populated with the appropriate constants and procedures.

use super::symtab::*;
use crate::lexer::Span;

/// Register all intrinsic module scopes in the symbol table.
/// Called once during semantic analysis initialization.
pub fn register_intrinsic_modules(st: &mut SymbolTable) {
    register_iso_c_binding(st);
    register_iso_fortran_env(st);
}

fn builtin_span() -> Span {
    let pos = crate::lexer::Position { line: 0, col: 0 };
    Span { start: pos, end: pos, file_id: 0 }
}

fn insert_param(st: &mut SymbolTable, mod_id: ScopeId, name: &str, ti: TypeInfo) {
    let span = builtin_span();
    st.scope_mut(mod_id).symbols.insert(name.to_lowercase(), Symbol {
        name: name.to_string(),
        kind: SymbolKind::Parameter,
        type_info: Some(ti),
        attrs: SymbolAttrs { parameter: true, ..Default::default() },
        defined_at: span,
        scope: mod_id,
    });
}

fn insert_type(st: &mut SymbolTable, mod_id: ScopeId, name: &str) {
    let span = builtin_span();
    st.scope_mut(mod_id).symbols.insert(name.to_lowercase(), Symbol {
        name: name.to_string(),
        kind: SymbolKind::DerivedType,
        type_info: Some(TypeInfo::Derived(name.to_string())),
        attrs: Default::default(),
        defined_at: span,
        scope: mod_id,
    });
}

fn insert_proc(st: &mut SymbolTable, mod_id: ScopeId, name: &str) {
    let span = builtin_span();
    st.scope_mut(mod_id).symbols.insert(name.to_lowercase(), Symbol {
        name: name.to_string(),
        kind: SymbolKind::IntrinsicProc,
        type_info: None,
        attrs: SymbolAttrs { intrinsic: true, ..Default::default() },
        defined_at: span,
        scope: mod_id,
    });
}

/// Populate the iso_c_binding module scope.
fn register_iso_c_binding(st: &mut SymbolTable) {
    let m = st.push_scope(ScopeKind::Module("iso_c_binding".into()));

    // ---- Integer kind parameters (ARM64 macOS LP64) ----
    let ik = |k: u8| TypeInfo::Integer { kind: Some(k) };
    for (name, kind) in [
        ("c_int", 4), ("c_short", 2), ("c_long", 8), ("c_long_long", 8),
        ("c_signed_char", 1),
        ("c_int8_t", 1), ("c_int16_t", 2), ("c_int32_t", 4), ("c_int64_t", 8),
        ("c_size_t", 8), ("c_intptr_t", 8), ("c_ptrdiff_t", 8),
    ] {
        insert_param(st, m, name, ik(kind));
    }

    // ---- Real kind parameters ----
    let rk = |k: u8| TypeInfo::Real { kind: Some(k) };
    for (name, kind) in [
        ("c_float", 4), ("c_double", 8), ("c_long_double", 8),
    ] {
        insert_param(st, m, name, rk(kind));
    }

    // ---- Character and logical kinds ----
    insert_param(st, m, "c_char", TypeInfo::Character { len: Some(1), kind: Some(1) });
    insert_param(st, m, "c_bool", TypeInfo::Logical { kind: Some(1) });

    // ---- Character constants (c_null_char, etc.) ----
    let ck = TypeInfo::Character { len: Some(1), kind: Some(1) };
    for name in [
        "c_null_char", "c_alert", "c_backspace", "c_form_feed",
        "c_new_line", "c_carriage_return", "c_horizontal_tab", "c_vertical_tab",
    ] {
        insert_param(st, m, name, ck.clone());
    }

    // ---- Pointer types ----
    insert_type(st, m, "c_ptr");
    insert_type(st, m, "c_funptr");

    // ---- Null pointer constants ----
    insert_param(st, m, "c_null_ptr", ik(8));
    insert_param(st, m, "c_null_funptr", ik(8));

    // ---- Procedures ----
    for name in ["c_loc", "c_funloc", "c_f_pointer", "c_f_procpointer", "c_associated", "c_sizeof"] {
        insert_proc(st, m, name);
    }

    st.pop_scope();
}

/// Populate the iso_fortran_env module scope.
fn register_iso_fortran_env(st: &mut SymbolTable) {
    let m = st.push_scope(ScopeKind::Module("iso_fortran_env".into()));

    let ik = |k: u8| TypeInfo::Integer { kind: Some(k) };

    // Standard I/O unit numbers.
    for name in ["input_unit", "output_unit", "error_unit", "iostat_end", "iostat_eor"] {
        insert_param(st, m, name, ik(4));
    }

    // Kind parameters.
    for name in ["int8", "int16", "int32", "int64", "real32", "real64",
                 "character_kinds", "integer_kinds", "logical_kinds", "real_kinds"] {
        insert_param(st, m, name, ik(4));
    }

    st.pop_scope();
}
