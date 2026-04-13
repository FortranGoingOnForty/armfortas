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
    register_ieee_stubs(st);
}

fn builtin_span() -> Span {
    let pos = crate::lexer::Position { line: 0, col: 0 };
    Span { start: pos, end: pos, file_id: 0 }
}

fn insert_param(st: &mut SymbolTable, mod_id: ScopeId, name: &str, ti: TypeInfo) {
    insert_param_val(st, mod_id, name, ti, None);
}

fn insert_param_val(st: &mut SymbolTable, mod_id: ScopeId, name: &str, ti: TypeInfo, val: Option<i64>) {
    let span = builtin_span();
    st.scope_mut(mod_id).symbols.insert(name.to_lowercase(), Symbol {
        name: name.to_string(),
        kind: SymbolKind::Parameter,
        type_info: Some(ti),
        attrs: SymbolAttrs { parameter: true, ..Default::default() },
        defined_at: span,
        scope: mod_id,
        arg_names: vec![],
        const_value: val,
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
        arg_names: vec![],
        const_value: None,
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
        arg_names: vec![],
        const_value: None,
    });
}

/// Populate the iso_c_binding module scope.
fn register_iso_c_binding(st: &mut SymbolTable) {
    let m = st.push_scope(ScopeKind::Module("iso_c_binding".into()));

    // ---- Integer kind parameters (ARM64 macOS LP64) ----
    // Each constant's VALUE is the kind number (e.g., c_int = 4 means kind=4 = 4 bytes).
    let ik = |k: u8| TypeInfo::Integer { kind: Some(k) };
    for (name, kind) in [
        ("c_int", 4u8), ("c_short", 2), ("c_long", 8), ("c_long_long", 8),
        ("c_signed_char", 1),
        ("c_int8_t", 1), ("c_int16_t", 2), ("c_int32_t", 4), ("c_int64_t", 8),
        ("c_size_t", 8), ("c_intptr_t", 8), ("c_ptrdiff_t", 8),
    ] {
        insert_param_val(st, m, name, ik(4), Some(kind as i64));
    }

    // ---- Real kind parameters ----
    for (name, kind) in [
        ("c_float", 4u8), ("c_double", 8), ("c_long_double", 8),
    ] {
        insert_param_val(st, m, name, TypeInfo::Integer { kind: Some(4) }, Some(kind as i64));
    }

    // ---- Complex kind parameters ----
    for (name, kind) in [
        ("c_float_complex", 4u8), ("c_double_complex", 8), ("c_long_double_complex", 8),
    ] {
        insert_param_val(st, m, name, TypeInfo::Integer { kind: Some(4) }, Some(kind as i64));
    }

    // ---- Character and logical kinds ----
    insert_param(st, m, "c_char", TypeInfo::Character { len: Some(1), kind: Some(1) });
    insert_param(st, m, "c_bool", TypeInfo::Logical { kind: Some(1) });

    // ---- Character constants (c_null_char, etc.) ----
    // Each constant's value is its ASCII byte code.
    let ck = TypeInfo::Character { len: Some(1), kind: Some(1) };
    for (name, ascii) in [
        ("c_null_char", 0i64),
        ("c_alert", 7),
        ("c_backspace", 8),
        ("c_horizontal_tab", 9),
        ("c_new_line", 10),
        ("c_vertical_tab", 11),
        ("c_form_feed", 12),
        ("c_carriage_return", 13),
    ] {
        insert_param_val(st, m, name, ck.clone(), Some(ascii));
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

    let ik4 = TypeInfo::Integer { kind: Some(4) };

    // Standard I/O unit numbers — actual values.
    insert_param_val(st, m, "input_unit", ik4.clone(), Some(5));
    insert_param_val(st, m, "output_unit", ik4.clone(), Some(6));
    insert_param_val(st, m, "error_unit", ik4.clone(), Some(0));
    insert_param_val(st, m, "iostat_end", ik4.clone(), Some(-1));
    insert_param_val(st, m, "iostat_eor", ik4.clone(), Some(-2));

    // Kind parameters — values are the kind numbers themselves.
    insert_param_val(st, m, "int8", ik4.clone(), Some(1));
    insert_param_val(st, m, "int16", ik4.clone(), Some(2));
    insert_param_val(st, m, "int32", ik4.clone(), Some(4));
    insert_param_val(st, m, "int64", ik4.clone(), Some(8));
    insert_param_val(st, m, "real32", ik4.clone(), Some(4));
    insert_param_val(st, m, "real64", ik4.clone(), Some(8));
    insert_param_val(st, m, "real128", ik4.clone(), Some(16));
    insert_param_val(st, m, "character_kinds", ik4.clone(), Some(1));
    insert_param_val(st, m, "integer_kinds", ik4.clone(), Some(4));
    insert_param_val(st, m, "logical_kinds", ik4.clone(), Some(4));
    insert_param_val(st, m, "real_kinds", ik4, Some(4));

    // Inquiry functions — lowered to string constants by the compiler.
    insert_proc(st, m, "compiler_version");
    insert_proc(st, m, "compiler_options");

    st.pop_scope();
}

/// Register stub IEEE module scopes so `USE ieee_arithmetic` etc.
/// don't produce a "module not found" error.  The procedures
/// themselves are not yet implemented (sprint 30.7).
fn register_ieee_stubs(st: &mut SymbolTable) {
    for name in ["ieee_arithmetic", "ieee_exceptions", "ieee_features"] {
        let m = st.push_scope(ScopeKind::Module(name.into()));
        // Populate with commonly-referenced symbols so USE ONLY
        // doesn't fail on standard names.
        match name {
            "ieee_arithmetic" => {
                insert_type(st, m, "ieee_class_type");
                insert_type(st, m, "ieee_round_type");
                insert_proc(st, m, "ieee_is_nan");
                insert_proc(st, m, "ieee_is_finite");
                insert_proc(st, m, "ieee_value");
                insert_proc(st, m, "ieee_selected_real_kind");
                insert_proc(st, m, "ieee_support_datatype");
                insert_proc(st, m, "ieee_support_denormal");
            }
            "ieee_exceptions" => {
                insert_type(st, m, "ieee_flag_type");
                insert_type(st, m, "ieee_status_type");
                insert_proc(st, m, "ieee_get_flag");
                insert_proc(st, m, "ieee_set_flag");
                insert_proc(st, m, "ieee_get_halting_mode");
                insert_proc(st, m, "ieee_set_halting_mode");
            }
            "ieee_features" => {
                for feat in ["ieee_datatype", "ieee_denormal", "ieee_divide",
                             "ieee_halting", "ieee_inexact_flag", "ieee_inf",
                             "ieee_invalid_flag", "ieee_nan", "ieee_rounding",
                             "ieee_sqrt", "ieee_underflow_flag"] {
                    insert_param(st, m, feat, TypeInfo::Logical { kind: Some(4) });
                }
            }
            _ => {}
        }
        st.pop_scope();
    }
}
