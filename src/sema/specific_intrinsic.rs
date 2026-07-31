//! Characteristics and ABI adapters for unrestricted specific intrinsics.
//!
//! A specific intrinsic may be a procedure-pointer target, but its ordinary
//! lowering often uses a value-taking C/libm entry point. Procedure pointers
//! use the Fortran by-reference ABI, so lowering must take the address of the
//! matching `afs_specific_*` adapter instead of the direct intrinsic callee.
//! Keep the accepted characteristics and adapter symbol in this single table.

#[derive(Clone, Copy)]
pub(crate) enum SpecificIntrinsicType {
    Integer,
    Real,
    DoublePrecision,
    Complex,
    Character,
}

#[derive(Clone, Copy)]
pub(crate) struct SpecificIntrinsic {
    pub(crate) arguments: &'static [SpecificIntrinsicType],
    pub(crate) result: SpecificIntrinsicType,
    pub(crate) wrapper_symbol: &'static str,
}

use SpecificIntrinsicType::{Character, Complex, DoublePrecision, Integer, Real};

const REAL_1: &[SpecificIntrinsicType] = &[Real];
const REAL_2: &[SpecificIntrinsicType] = &[Real, Real];
const DOUBLE_1: &[SpecificIntrinsicType] = &[DoublePrecision];
const DOUBLE_2: &[SpecificIntrinsicType] = &[DoublePrecision, DoublePrecision];
const COMPLEX_1: &[SpecificIntrinsicType] = &[Complex];
const INTEGER_1: &[SpecificIntrinsicType] = &[Integer];
const INTEGER_2: &[SpecificIntrinsicType] = &[Integer, Integer];
const CHARACTER_1: &[SpecificIntrinsicType] = &[Character];
const CHARACTER_2: &[SpecificIntrinsicType] = &[Character, Character];

pub(crate) fn specific_intrinsic(name: &str) -> Option<SpecificIntrinsic> {
    let definition = match name.to_ascii_lowercase().as_str() {
        "abs" => (REAL_1, Real, "afs_specific_abs_r4"),
        "acos" => (REAL_1, Real, "afs_specific_acos_r4"),
        "aint" => (REAL_1, Real, "afs_specific_aint_r4"),
        "alog" => (REAL_1, Real, "afs_specific_log_r4"),
        "alog10" => (REAL_1, Real, "afs_specific_log10_r4"),
        "anint" => (REAL_1, Real, "afs_specific_anint_r4"),
        "asin" => (REAL_1, Real, "afs_specific_asin_r4"),
        "atan" => (REAL_1, Real, "afs_specific_atan_r4"),
        "cos" => (REAL_1, Real, "afs_specific_cos_r4"),
        "cosh" => (REAL_1, Real, "afs_specific_cosh_r4"),
        "exp" => (REAL_1, Real, "afs_specific_exp_r4"),
        "sin" => (REAL_1, Real, "afs_specific_sin_r4"),
        "sinh" => (REAL_1, Real, "afs_specific_sinh_r4"),
        "sqrt" => (REAL_1, Real, "afs_specific_sqrt_r4"),
        "tan" => (REAL_1, Real, "afs_specific_tan_r4"),
        "tanh" => (REAL_1, Real, "afs_specific_tanh_r4"),
        "amod" => (REAL_2, Real, "afs_specific_mod_r4"),
        "atan2" => (REAL_2, Real, "afs_specific_atan2_r4"),
        "dim" => (REAL_2, Real, "afs_specific_dim_r4"),
        "sign" => (REAL_2, Real, "afs_specific_sign_r4"),
        "aimag" => (COMPLEX_1, Real, "afs_specific_aimag_c4"),
        "cabs" => (COMPLEX_1, Real, "afs_specific_abs_c4"),
        "ccos" => (COMPLEX_1, Complex, "afs_specific_cos_c4"),
        "cexp" => (COMPLEX_1, Complex, "afs_specific_exp_c4"),
        "clog" => (COMPLEX_1, Complex, "afs_specific_log_c4"),
        "conjg" => (COMPLEX_1, Complex, "afs_specific_conjg_c4"),
        "csin" => (COMPLEX_1, Complex, "afs_specific_sin_c4"),
        "csqrt" => (COMPLEX_1, Complex, "afs_specific_sqrt_c4"),
        "dabs" => (DOUBLE_1, DoublePrecision, "afs_specific_abs_r8"),
        "dacos" => (DOUBLE_1, DoublePrecision, "afs_specific_acos_r8"),
        "dasin" => (DOUBLE_1, DoublePrecision, "afs_specific_asin_r8"),
        "datan" => (DOUBLE_1, DoublePrecision, "afs_specific_atan_r8"),
        "dcos" => (DOUBLE_1, DoublePrecision, "afs_specific_cos_r8"),
        "dcosh" => (DOUBLE_1, DoublePrecision, "afs_specific_cosh_r8"),
        "dexp" => (DOUBLE_1, DoublePrecision, "afs_specific_exp_r8"),
        "dint" => (DOUBLE_1, DoublePrecision, "afs_specific_aint_r8"),
        "dlog" => (DOUBLE_1, DoublePrecision, "afs_specific_log_r8"),
        "dlog10" => (DOUBLE_1, DoublePrecision, "afs_specific_log10_r8"),
        "dnint" => (DOUBLE_1, DoublePrecision, "afs_specific_anint_r8"),
        "dsin" => (DOUBLE_1, DoublePrecision, "afs_specific_sin_r8"),
        "dsinh" => (DOUBLE_1, DoublePrecision, "afs_specific_sinh_r8"),
        "dsqrt" => (DOUBLE_1, DoublePrecision, "afs_specific_sqrt_r8"),
        "dtan" => (DOUBLE_1, DoublePrecision, "afs_specific_tan_r8"),
        "dtanh" => (DOUBLE_1, DoublePrecision, "afs_specific_tanh_r8"),
        "datan2" => (DOUBLE_2, DoublePrecision, "afs_specific_atan2_r8"),
        "ddim" => (DOUBLE_2, DoublePrecision, "afs_specific_dim_r8"),
        "dmod" => (DOUBLE_2, DoublePrecision, "afs_specific_mod_r8"),
        "dsign" => (DOUBLE_2, DoublePrecision, "afs_specific_sign_r8"),
        "dprod" => (REAL_2, DoublePrecision, "afs_specific_dprod_r4"),
        "iabs" => (INTEGER_1, Integer, "afs_specific_abs_i4"),
        "idim" => (INTEGER_2, Integer, "afs_specific_dim_i4"),
        "isign" => (INTEGER_2, Integer, "afs_specific_sign_i4"),
        "mod" => (INTEGER_2, Integer, "afs_specific_mod_i4"),
        "idnint" => (DOUBLE_1, Integer, "afs_specific_nint_r8_i4"),
        "nint" => (REAL_1, Integer, "afs_specific_nint_r4_i4"),
        "index" => (CHARACTER_2, Integer, "afs_specific_index_ch1_i4"),
        "len" => (CHARACTER_1, Integer, "afs_specific_len_ch1_i4"),
        _ => return None,
    };
    Some(SpecificIntrinsic {
        arguments: definition.0,
        result: definition.1,
        wrapper_symbol: definition.2,
    })
}

pub(crate) fn specific_intrinsic_wrapper_symbol(name: &str) -> Option<&'static str> {
    specific_intrinsic(name).map(|definition| definition.wrapper_symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_specific_has_a_stable_adapter_symbol() {
        for name in [
            "abs", "acos", "aint", "alog", "alog10", "anint", "asin", "atan", "cos", "cosh", "exp",
            "sin", "sinh", "sqrt", "tan", "tanh", "amod", "atan2", "dim", "sign", "aimag", "cabs",
            "ccos", "cexp", "clog", "conjg", "csin", "csqrt", "dabs", "dacos", "dasin", "datan",
            "dcos", "dcosh", "dexp", "dint", "dlog", "dlog10", "dnint", "dsin", "dsinh", "dsqrt",
            "dtan", "dtanh", "datan2", "ddim", "dmod", "dsign", "dprod", "iabs", "idim", "isign",
            "mod", "idnint", "nint", "index", "len",
        ] {
            let definition = specific_intrinsic(name)
                .unwrap_or_else(|| panic!("missing specific intrinsic definition for {name}"));
            assert!(
                definition.wrapper_symbol.starts_with("afs_specific_"),
                "unstable adapter namespace for {name}: {}",
                definition.wrapper_symbol
            );
        }
    }
}
