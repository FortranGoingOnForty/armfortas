//! Fortran-ABI adapters for specific intrinsic procedure targets.
//!
//! Ordinary intrinsic expressions are lowered to value-taking runtime or
//! libm calls. A Fortran procedure pointer, however, passes non-VALUE dummy
//! arguments by reference. These exported adapters are the only addresses
//! stored when a specific intrinsic is used as a procedure-pointer target.

macro_rules! unary_adapter {
    ($name:ident, $ty:ty, $body:expr) => {
        #[no_mangle]
        pub extern "C" fn $name(value: *const $ty) -> $ty {
            let value = unsafe { *value };
            $body(value)
        }
    };
}

macro_rules! binary_adapter {
    ($name:ident, $ty:ty, $body:expr) => {
        #[no_mangle]
        pub extern "C" fn $name(lhs: *const $ty, rhs: *const $ty) -> $ty {
            let lhs = unsafe { *lhs };
            let rhs = unsafe { *rhs };
            $body(lhs, rhs)
        }
    };
}

unary_adapter!(afs_specific_abs_r4, f32, f32::abs);
unary_adapter!(afs_specific_acos_r4, f32, f32::acos);
unary_adapter!(afs_specific_aint_r4, f32, f32::trunc);
unary_adapter!(afs_specific_log_r4, f32, f32::ln);
unary_adapter!(afs_specific_log10_r4, f32, f32::log10);
unary_adapter!(afs_specific_anint_r4, f32, f32::round);
unary_adapter!(afs_specific_asin_r4, f32, f32::asin);
unary_adapter!(afs_specific_atan_r4, f32, f32::atan);
unary_adapter!(afs_specific_cos_r4, f32, f32::cos);
unary_adapter!(afs_specific_cosh_r4, f32, f32::cosh);
unary_adapter!(afs_specific_exp_r4, f32, f32::exp);
unary_adapter!(afs_specific_sin_r4, f32, f32::sin);
unary_adapter!(afs_specific_sinh_r4, f32, f32::sinh);
unary_adapter!(afs_specific_sqrt_r4, f32, f32::sqrt);
unary_adapter!(afs_specific_tan_r4, f32, f32::tan);
unary_adapter!(afs_specific_tanh_r4, f32, f32::tanh);

binary_adapter!(afs_specific_mod_r4, f32, |lhs: f32, rhs: f32| lhs % rhs);
binary_adapter!(afs_specific_atan2_r4, f32, f32::atan2);
binary_adapter!(afs_specific_dim_r4, f32, |lhs: f32, rhs: f32| {
    (lhs - rhs).max(0.0)
});
binary_adapter!(afs_specific_sign_r4, f32, |lhs: f32, rhs: f32| {
    lhs.abs().copysign(rhs)
});

unary_adapter!(afs_specific_abs_r8, f64, f64::abs);
unary_adapter!(afs_specific_acos_r8, f64, f64::acos);
unary_adapter!(afs_specific_aint_r8, f64, f64::trunc);
unary_adapter!(afs_specific_log_r8, f64, f64::ln);
unary_adapter!(afs_specific_log10_r8, f64, f64::log10);
unary_adapter!(afs_specific_anint_r8, f64, f64::round);
unary_adapter!(afs_specific_asin_r8, f64, f64::asin);
unary_adapter!(afs_specific_atan_r8, f64, f64::atan);
unary_adapter!(afs_specific_cos_r8, f64, f64::cos);
unary_adapter!(afs_specific_cosh_r8, f64, f64::cosh);
unary_adapter!(afs_specific_exp_r8, f64, f64::exp);
unary_adapter!(afs_specific_sin_r8, f64, f64::sin);
unary_adapter!(afs_specific_sinh_r8, f64, f64::sinh);
unary_adapter!(afs_specific_sqrt_r8, f64, f64::sqrt);
unary_adapter!(afs_specific_tan_r8, f64, f64::tan);
unary_adapter!(afs_specific_tanh_r8, f64, f64::tanh);

binary_adapter!(afs_specific_mod_r8, f64, |lhs: f64, rhs: f64| lhs % rhs);
binary_adapter!(afs_specific_atan2_r8, f64, f64::atan2);
binary_adapter!(afs_specific_dim_r8, f64, |lhs: f64, rhs: f64| {
    (lhs - rhs).max(0.0)
});
binary_adapter!(afs_specific_sign_r8, f64, |lhs: f64, rhs: f64| {
    lhs.abs().copysign(rhs)
});

#[no_mangle]
pub extern "C" fn afs_specific_dprod_r4(lhs: *const f32, rhs: *const f32) -> f64 {
    let lhs = unsafe { *lhs };
    let rhs = unsafe { *rhs };
    f64::from(lhs) * f64::from(rhs)
}

#[no_mangle]
pub extern "C" fn afs_specific_abs_i4(value: *const i32) -> i32 {
    unsafe { *value }.wrapping_abs()
}

#[no_mangle]
pub extern "C" fn afs_specific_dim_i4(lhs: *const i32, rhs: *const i32) -> i32 {
    unsafe { *lhs }.wrapping_sub(unsafe { *rhs }).max(0)
}

#[no_mangle]
pub extern "C" fn afs_specific_sign_i4(lhs: *const i32, rhs: *const i32) -> i32 {
    let magnitude = unsafe { *lhs }.wrapping_abs();
    if unsafe { *rhs } < 0 {
        magnitude.wrapping_neg()
    } else {
        magnitude
    }
}

#[no_mangle]
pub extern "C" fn afs_specific_mod_i4(lhs: *const i32, rhs: *const i32) -> i32 {
    unsafe { *lhs }.wrapping_rem(unsafe { *rhs })
}

#[no_mangle]
pub extern "C" fn afs_specific_nint_r4_i4(value: *const f32) -> i32 {
    unsafe { *value }.round() as i32
}

#[no_mangle]
pub extern "C" fn afs_specific_nint_r8_i4(value: *const f64) -> i32 {
    unsafe { *value }.round() as i32
}

fn load_complex(value: *const [f32; 2]) -> (f32, f32) {
    let [real, imaginary] = unsafe { *value };
    (real, imaginary)
}

fn store_complex(result: *mut [f32; 2], real: f32, imaginary: f32) {
    unsafe {
        *result = [real, imaginary];
    }
}

#[no_mangle]
pub extern "C" fn afs_specific_aimag_c4(value: *const [f32; 2]) -> f32 {
    load_complex(value).1
}

#[no_mangle]
pub extern "C" fn afs_specific_abs_c4(value: *const [f32; 2]) -> f32 {
    let (real, imaginary) = load_complex(value);
    real.hypot(imaginary)
}

#[no_mangle]
pub extern "C" fn afs_specific_conjg_c4(result: *mut [f32; 2], value: *const [f32; 2]) {
    let (real, imaginary) = load_complex(value);
    store_complex(result, real, -imaginary);
}

#[no_mangle]
pub extern "C" fn afs_specific_exp_c4(result: *mut [f32; 2], value: *const [f32; 2]) {
    let (real, imaginary) = load_complex(value);
    let magnitude = real.exp();
    store_complex(
        result,
        magnitude * imaginary.cos(),
        magnitude * imaginary.sin(),
    );
}

#[no_mangle]
pub extern "C" fn afs_specific_log_c4(result: *mut [f32; 2], value: *const [f32; 2]) {
    let (real, imaginary) = load_complex(value);
    store_complex(result, real.hypot(imaginary).ln(), imaginary.atan2(real));
}

#[no_mangle]
pub extern "C" fn afs_specific_sin_c4(result: *mut [f32; 2], value: *const [f32; 2]) {
    let (real, imaginary) = load_complex(value);
    store_complex(
        result,
        real.sin() * imaginary.cosh(),
        real.cos() * imaginary.sinh(),
    );
}

#[no_mangle]
pub extern "C" fn afs_specific_cos_c4(result: *mut [f32; 2], value: *const [f32; 2]) {
    let (real, imaginary) = load_complex(value);
    store_complex(
        result,
        real.cos() * imaginary.cosh(),
        -real.sin() * imaginary.sinh(),
    );
}

#[no_mangle]
pub extern "C" fn afs_specific_sqrt_c4(result: *mut [f32; 2], value: *const [f32; 2]) {
    let (real, imaginary) = load_complex(value);
    let magnitude = real.hypot(imaginary);
    let result_real = ((magnitude + real) * 0.5).sqrt();
    let result_imaginary = ((magnitude - real) * 0.5).sqrt().copysign(imaginary);
    store_complex(result, result_real, result_imaginary);
}

fn character_bytes<'a>(slot: *const *const u8, len: i64) -> &'a [u8] {
    if slot.is_null() || len <= 0 {
        return &[];
    }
    let data = unsafe { *slot };
    if data.is_null() {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(data, len as usize) }
}

#[no_mangle]
pub extern "C" fn afs_specific_len_ch1_i4(_value: *const *const u8, value_len: i64) -> i32 {
    value_len.clamp(0, i64::from(i32::MAX)) as i32
}

#[no_mangle]
pub extern "C" fn afs_specific_index_ch1_i4(
    string: *const *const u8,
    substring: *const *const u8,
    string_len: i64,
    substring_len: i64,
) -> i32 {
    let string = character_bytes(string, string_len);
    let substring = character_bytes(substring, substring_len);
    if substring.is_empty() {
        return 1;
    }
    string
        .windows(substring.len())
        .position(|window| window == substring)
        .and_then(|position| i32::try_from(position + 1).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_adapters_read_fortran_reference_arguments() {
        assert!((afs_specific_sin_r4(&0.5) - 0.5_f32.sin()).abs() < 1.0e-6);
        assert_eq!(afs_specific_dim_i4(&7, &3), 4);
        assert_eq!(afs_specific_dprod_r4(&2.0, &4.0), 8.0);
    }

    #[test]
    fn complex_adapters_write_the_hidden_result_buffer() {
        let input = [1.5, -2.0];
        let mut output = [0.0, 0.0];
        afs_specific_conjg_c4(&mut output, &input);
        assert_eq!(output, [1.5, 2.0]);
    }

    #[test]
    fn character_adapters_honor_hidden_lengths() {
        let string = b"compiler";
        let substring = b"pile";
        let string_ptr = string.as_ptr();
        let substring_ptr = substring.as_ptr();
        assert_eq!(afs_specific_len_ch1_i4(&string_ptr, 8), 8);
        assert_eq!(
            afs_specific_index_ch1_i4(&string_ptr, &substring_ptr, 8, 4),
            4
        );
    }
}
