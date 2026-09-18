//! Built-in intrinsic modules.
//!
//! These modules are constructed programmatically rather than parsed
//! from source. When `USE iso_c_binding` is encountered, the symbol
//! table is populated with the appropriate constants and procedures.

use super::symtab::*;
use crate::lexer::Span;

const ISO_C_BINDING: &str = "iso_c_binding";
const ISO_FORTRAN_ENV: &str = "iso_fortran_env";
const IEEE_MODULES: [&str; 3] = ["ieee_arithmetic", "ieee_exceptions", "ieee_features"];
const OMP_MODULES: [&str; 2] = ["omp_lib", "omp_lib_kinds"];

pub fn is_intrinsic_module(name: &str) -> bool {
    name.eq_ignore_ascii_case(ISO_C_BINDING)
        || name.eq_ignore_ascii_case(ISO_FORTRAN_ENV)
        || IEEE_MODULES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
        || OMP_MODULES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Register all intrinsic module scopes in the symbol table.
/// Called once during semantic analysis initialization.
pub fn register_intrinsic_modules(st: &mut SymbolTable) {
    register_iso_c_binding(st);
    register_iso_fortran_env(st);
    register_ieee_modules(st);
    register_openmp_modules(st);
}

fn builtin_span() -> Span {
    let pos = crate::lexer::Position { line: 0, col: 0 };
    Span {
        start: pos,
        end: pos,
        file_id: 0,
    }
}

fn insert_param(st: &mut SymbolTable, mod_id: ScopeId, name: &str, ti: TypeInfo) {
    insert_param_val(st, mod_id, name, ti, None);
}

fn insert_param_val(
    st: &mut SymbolTable,
    mod_id: ScopeId,
    name: &str,
    ti: TypeInfo,
    val: Option<i64>,
) {
    let span = builtin_span();
    let const_char_value = match (&ti, val) {
        (TypeInfo::Character { .. }, Some(v)) if (0..=255).contains(&v) => {
            Some((v as u8 as char).to_string())
        }
        _ => None,
    };
    st.scope_mut(mod_id).symbols.insert(
        name.to_lowercase(),
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::Parameter,
            type_info: Some(ti),
            attrs: SymbolAttrs {
                parameter: true,
                ..Default::default()
            },
            defined_at: span,
            scope: mod_id,
            arg_names: vec![],
            const_value: val,
            const_char_value,
        },
    );
}

fn insert_type(st: &mut SymbolTable, mod_id: ScopeId, name: &str) {
    let span = builtin_span();
    st.scope_mut(mod_id).symbols.insert(
        name.to_lowercase(),
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::DerivedType,
            type_info: Some(TypeInfo::Derived(name.to_string())),
            attrs: Default::default(),
            defined_at: span,
            scope: mod_id,
            arg_names: vec![],
            const_value: None,
            const_char_value: None,
        },
    );
}

fn insert_proc(st: &mut SymbolTable, mod_id: ScopeId, name: &str) {
    let span = builtin_span();
    st.scope_mut(mod_id).symbols.insert(
        name.to_lowercase(),
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::IntrinsicProc,
            type_info: None,
            attrs: SymbolAttrs {
                intrinsic: true,
                ..Default::default()
            },
            defined_at: span,
            scope: mod_id,
            arg_names: vec![],
            const_value: None,
            const_char_value: None,
        },
    );
}

/// Populate the iso_c_binding module scope.
fn register_iso_c_binding(st: &mut SymbolTable) {
    let m = st.push_intrinsic_module_scope(ISO_C_BINDING);

    // ---- Integer kind parameters (ARM64 macOS LP64) ----
    // Each constant's VALUE is the kind number (e.g., c_int = 4 means kind=4 = 4 bytes).
    let ik = |k: u8| TypeInfo::Integer { kind: Some(k) };
    for (name, kind) in [
        ("c_int", 4u8),
        ("c_short", 2),
        ("c_long", 8),
        ("c_long_long", 8),
        ("c_signed_char", 1),
        ("c_int8_t", 1),
        ("c_int16_t", 2),
        ("c_int32_t", 4),
        ("c_int64_t", 8),
        ("c_size_t", 8),
        ("c_intptr_t", 8),
        ("c_ptrdiff_t", 8),
    ] {
        insert_param_val(st, m, name, ik(4), Some(kind as i64));
    }

    // ---- Real kind parameters ----
    for (name, kind) in [("c_float", 4u8), ("c_double", 8), ("c_long_double", 8)] {
        insert_param_val(
            st,
            m,
            name,
            TypeInfo::Integer { kind: Some(4) },
            Some(kind as i64),
        );
    }

    // ---- Complex kind parameters ----
    for (name, kind) in [
        ("c_float_complex", 4u8),
        ("c_double_complex", 8),
        ("c_long_double_complex", 8),
    ] {
        insert_param_val(
            st,
            m,
            name,
            TypeInfo::Integer { kind: Some(4) },
            Some(kind as i64),
        );
    }

    // ---- Character and logical kinds ----
    // c_char is an integer kind parameter (value = 1), not a character type.
    insert_param_val(st, m, "c_char", ik(4), Some(1));
    insert_param_val(
        st,
        m,
        "c_bool",
        TypeInfo::Integer { kind: Some(4) },
        Some(1),
    );

    // ---- Character constants (c_null_char, etc.) ----
    // Each constant's value is its ASCII byte code.
    let ck = TypeInfo::Character {
        len: Some(1),
        kind: Some(1),
    };
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
    insert_param_val(
        st,
        m,
        "c_null_ptr",
        TypeInfo::Derived("c_ptr".into()),
        Some(0),
    );
    insert_param_val(
        st,
        m,
        "c_null_funptr",
        TypeInfo::Derived("c_funptr".into()),
        Some(0),
    );

    // ---- Procedures ----
    for name in [
        "c_loc",
        "c_funloc",
        "c_f_pointer",
        "c_f_procpointer",
        "c_associated",
        "c_sizeof",
        // F2023 18.2.3: C string interop.
        "c_f_strpointer",
        "f_c_string",
    ] {
        insert_proc(st, m, name);
    }

    st.pop_scope();
}

/// Populate the iso_fortran_env module scope.
fn register_iso_fortran_env(st: &mut SymbolTable) {
    let m = st.push_intrinsic_module_scope(ISO_FORTRAN_ENV);

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
    // F2023 16.10.2: logical kind constants (kind = bit width / 8) and
    // the 16-bit real constant. armfortas has no 16-bit real, so
    // REAL16 takes the standard's -2 sentinel ("no kind of this size,
    // but a larger size exists"; 16.10.2.27) since real32/64 exist.
    insert_param_val(st, m, "logical8", ik4.clone(), Some(1));
    insert_param_val(st, m, "logical16", ik4.clone(), Some(2));
    insert_param_val(st, m, "logical32", ik4.clone(), Some(4));
    insert_param_val(st, m, "logical64", ik4.clone(), Some(8));
    insert_param_val(st, m, "real16", ik4.clone(), Some(-2));
    insert_param_val(st, m, "character_kinds", ik4.clone(), Some(1));
    insert_param_val(st, m, "integer_kinds", ik4.clone(), Some(4));
    insert_param_val(st, m, "logical_kinds", ik4.clone(), Some(4));
    insert_param_val(st, m, "real_kinds", ik4.clone(), Some(4));

    // Storage size constants (F2008).  Values reflect armfortas's
    // ARM64/macOS layout: one-byte default character (Fortran wide
    // characters and EBCDIC are not used), 32-bit default integer/real,
    // 8-bit file storage units.  stdlib relies on
    // `character_storage_size`/`bit_size(0_int8)` to compute byte
    // counts for `transfer(...)` calls — without this entry, the
    // module parameter folded to 0 and `transfer(value, mold,
    // bytes_char * len(value))` requested a zero-byte copy, leaving
    // hashmap key buffers empty and downstream key compares wrong.
    insert_param_val(st, m, "character_storage_size", ik4.clone(), Some(8));
    insert_param_val(st, m, "file_storage_size", ik4.clone(), Some(8));
    insert_param_val(st, m, "numeric_storage_size", ik4.clone(), Some(32));
    // Coarray stat constants (F2008/F2018).
    insert_param_val(st, m, "stat_stopped_image", ik4.clone(), Some(-3));
    insert_param_val(st, m, "stat_failed_image", ik4.clone(), Some(-4));
    insert_param_val(st, m, "stat_locked", ik4.clone(), Some(-5));
    insert_param_val(st, m, "stat_locked_other_image", ik4.clone(), Some(-6));
    insert_param_val(st, m, "stat_unlocked", ik4, Some(-7));

    // Inquiry functions — lowered to string constants by the compiler.
    insert_proc(st, m, "compiler_version");
    insert_proc(st, m, "compiler_options");

    st.pop_scope();
}

/// Register the IEEE module scopes (`ieee_arithmetic`,
/// `ieee_exceptions`, `ieee_features`) so `USE` resolves: the opaque
/// tag types, the class/round/flag named constants, and the procedure
/// names. The procedures are implemented in IR lowering
/// (`src/ir/lower/intrinsic.rs`, `intrinsic_sub.rs`) and the runtime
/// (`runtime/src/ieee.rs`); see the l09 support matrix there.
fn register_ieee_modules(st: &mut SymbolTable) {
    for name in IEEE_MODULES {
        let m = st.push_intrinsic_module_scope(name);
        // Populate with commonly-referenced symbols so USE ONLY
        // doesn't fail on standard names.
        match name {
            "ieee_arithmetic" => {
                insert_type(st, m, "ieee_class_type");
                insert_type(st, m, "ieee_round_type");
                let ik4 = TypeInfo::Integer { kind: Some(4) };
                for (name, value) in [
                    ("ieee_quiet_nan", 1),
                    ("ieee_positive_inf", 2),
                    ("ieee_negative_inf", 3),
                    ("ieee_signaling_nan", 4),
                    ("ieee_positive_zero", 5),
                    ("ieee_negative_zero", 6),
                    ("ieee_positive_denormal", 7),
                    ("ieee_negative_denormal", 8),
                    ("ieee_positive_normal", 9),
                    ("ieee_negative_normal", 10),
                    ("ieee_other_value", 11),
                ] {
                    insert_param_val(st, m, name, ik4.clone(), Some(value));
                }
                // Rounding-mode constants (ieee_round_type, modeled as
                // integer tags). Values match runtime/src/ieee.rs.
                for (name, value) in [
                    ("ieee_nearest", 0),
                    ("ieee_to_zero", 1),
                    ("ieee_up", 2),
                    ("ieee_down", 3),
                    ("ieee_away", 4),
                    ("ieee_other", 5),
                ] {
                    insert_param_val(st, m, name, ik4.clone(), Some(value));
                }
                insert_proc(st, m, "ieee_is_nan");
                insert_proc(st, m, "ieee_is_finite");
                insert_proc(st, m, "ieee_is_normal");
                insert_proc(st, m, "ieee_value");
                insert_proc(st, m, "ieee_class");
                for op in [
                    "ieee_max",
                    "ieee_min",
                    "ieee_max_mag",
                    "ieee_min_mag",
                    "ieee_max_num",
                    "ieee_min_num",
                    "ieee_max_num_mag",
                    "ieee_min_num_mag",
                ] {
                    insert_proc(st, m, op);
                }
                insert_proc(st, m, "ieee_selected_real_kind");
                insert_proc(st, m, "ieee_support_datatype");
                insert_proc(st, m, "ieee_support_denormal");
                insert_proc(st, m, "ieee_support_inf");
                insert_proc(st, m, "ieee_support_nan");
                insert_proc(st, m, "ieee_support_subnormal");
                insert_proc(st, m, "ieee_support_underflow_control");
                insert_proc(st, m, "ieee_support_halting");
                insert_proc(st, m, "ieee_support_flag");
                insert_proc(st, m, "ieee_support_standard");
                insert_proc(st, m, "ieee_support_rounding");
                insert_proc(st, m, "ieee_support_io");
                insert_proc(st, m, "ieee_support_divide");
                insert_proc(st, m, "ieee_support_sqrt");
                insert_proc(st, m, "ieee_get_rounding_mode");
                insert_proc(st, m, "ieee_set_rounding_mode");
                insert_proc(st, m, "ieee_get_underflow_mode");
                insert_proc(st, m, "ieee_set_underflow_mode");
                insert_proc(st, m, "ieee_copy_sign");
                insert_proc(st, m, "ieee_logb");
                insert_proc(st, m, "ieee_next_after");
                insert_proc(st, m, "ieee_rem");
                insert_proc(st, m, "ieee_rint");
                insert_proc(st, m, "ieee_scalb");
                insert_proc(st, m, "ieee_unordered");
                insert_proc(st, m, "ieee_fma");
            }
            "ieee_exceptions" => {
                insert_type(st, m, "ieee_flag_type");
                insert_type(st, m, "ieee_status_type");
                // Exception-flag constants (ieee_flag_type, integer tags).
                // Values match the flag bit indices in runtime/src/ieee.rs.
                let ik4 = TypeInfo::Integer { kind: Some(4) };
                for (name, value) in [
                    ("ieee_invalid", 1),
                    ("ieee_divide_by_zero", 2),
                    ("ieee_overflow", 3),
                    ("ieee_underflow", 4),
                    ("ieee_inexact", 5),
                ] {
                    insert_param_val(st, m, name, ik4.clone(), Some(value));
                }
                insert_proc(st, m, "ieee_get_flag");
                insert_proc(st, m, "ieee_set_flag");
                insert_proc(st, m, "ieee_get_status");
                insert_proc(st, m, "ieee_set_status");
                insert_proc(st, m, "ieee_get_halting_mode");
                insert_proc(st, m, "ieee_set_halting_mode");
            }
            "ieee_features" => {
                for feat in [
                    "ieee_datatype",
                    "ieee_denormal",
                    "ieee_divide",
                    "ieee_halting",
                    "ieee_inexact_flag",
                    "ieee_inf",
                    "ieee_invalid_flag",
                    "ieee_nan",
                    "ieee_rounding",
                    "ieee_sqrt",
                    "ieee_underflow_flag",
                ] {
                    insert_param(st, m, feat, TypeInfo::Logical { kind: Some(4) });
                }
            }
            _ => {}
        }
        st.pop_scope();
    }
}

/// Register the OpenMP Fortran modules supplied by the compiler.
///
/// `omp_lib` re-exports the kind and named constants from `omp_lib_kinds` in
/// the source form standardized by OpenMP. Built-in module scopes do not have
/// an internal USE-association phase, so populate the common constants in both
/// scopes. Procedure registration is deliberately limited to the runtime API
/// implemented by `runtime/src/openmp.rs`; unsupported API names must not look
/// available merely because another OpenMP implementation provides them.
fn register_openmp_modules(st: &mut SymbolTable) {
    for module in OMP_MODULES {
        let m = st.push_intrinsic_module_scope(module);
        register_openmp_constants(st, m);

        if module == "omp_lib" {
            insert_param_val(
                st,
                m,
                "openmp_version",
                TypeInfo::Integer { kind: Some(4) },
                Some(202111),
            );
            for name in [
                "omp_get_thread_num",
                "omp_get_num_threads",
                "omp_get_max_threads",
                "omp_in_parallel",
                "omp_set_num_threads",
                "omp_get_wtime",
                "omp_get_wtick",
            ] {
                insert_proc(st, m, name);
            }
        }

        st.pop_scope();
    }
}

fn register_openmp_constants(st: &mut SymbolTable, module: ScopeId) {
    let ik4 = TypeInfo::Integer { kind: Some(4) };

    // OpenMP 5.2 §3.1.1. The values are implementation-defined kind values;
    // these match ARMFORTAS's LP64 runtime ABI. The 16-byte depend object is
    // opaque to Fortran source and reserved for a later task-dependence ABI.
    for (name, value) in [
        ("omp_lock_kind", 8),
        ("omp_nest_lock_kind", 8),
        ("omp_sched_kind", 4),
        ("omp_proc_bind_kind", 4),
        ("omp_sync_hint_kind", 4),
        ("omp_lock_hint_kind", 4),
        ("omp_pause_resource_kind", 4),
        ("omp_allocator_handle_kind", 8),
        ("omp_alloctrait_key_kind", 4),
        ("omp_alloctrait_val_kind", 8),
        ("omp_memspace_handle_kind", 8),
        ("omp_depend_kind", 16),
        ("omp_event_handle_kind", 8),
        ("omp_interop_kind", 8),
        ("omp_interop_fr_kind", 4),
        ("omp_interop_property_kind", 4),
        ("omp_interop_rc_kind", 4),
    ] {
        insert_param_val(st, module, name, ik4.clone(), Some(value));
    }

    // Named constants required by the first scheduling and affinity clauses.
    for (name, value) in [
        ("omp_sched_static", 1),
        ("omp_sched_dynamic", 2),
        ("omp_sched_guided", 3),
        ("omp_sched_auto", 4),
        ("omp_proc_bind_false", 0),
        ("omp_proc_bind_true", 1),
        ("omp_proc_bind_primary", 2),
        ("omp_proc_bind_master", 2),
        ("omp_proc_bind_close", 3),
        ("omp_proc_bind_spread", 4),
        ("omp_sync_hint_none", 0),
        ("omp_lock_hint_none", 0),
        ("omp_sync_hint_uncontended", 1),
        ("omp_lock_hint_uncontended", 1),
        ("omp_sync_hint_contended", 2),
        ("omp_lock_hint_contended", 2),
        ("omp_sync_hint_nonspeculative", 4),
        ("omp_lock_hint_nonspeculative", 4),
        ("omp_sync_hint_speculative", 8),
        ("omp_lock_hint_speculative", 8),
        ("omp_pause_soft", 1),
        ("omp_pause_hard", 2),
    ] {
        insert_param_val(st, module, name, ik4.clone(), Some(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_both_openmp_modules_as_intrinsic() {
        assert!(is_intrinsic_module("OMP_LIB"));
        assert!(is_intrinsic_module("omp_lib_kinds"));
    }

    #[test]
    fn openmp_modules_publish_only_the_implemented_initial_surface() {
        let mut st = SymbolTable::new();
        register_intrinsic_modules(&mut st);

        let omp_lib = st
            .find_intrinsic_module_scope("omp_lib")
            .expect("omp_lib scope");
        let symbols = &st.scope(omp_lib).symbols;
        assert_eq!(symbols["openmp_version"].const_value, Some(202111));
        assert_eq!(symbols["omp_sched_dynamic"].const_value, Some(2));
        assert_eq!(symbols["omp_depend_kind"].const_value, Some(16));
        assert_eq!(
            symbols["omp_get_thread_num"].kind,
            SymbolKind::IntrinsicProc
        );
        assert!(!symbols.contains_key("omp_init_lock"));

        let kinds = st
            .find_intrinsic_module_scope("omp_lib_kinds")
            .expect("omp_lib_kinds scope");
        assert_eq!(
            st.scope(kinds).symbols["omp_allocator_handle_kind"].const_value,
            Some(8)
        );
        assert!(!st.scope(kinds).symbols.contains_key("openmp_version"));
    }
}
