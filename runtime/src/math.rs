//! F2023 degree and half-revolution trigonometric intrinsics.
//!
//! These families exist for EXACTNESS at the special angles, which a
//! naive `(x * PI / 180).sin()` cannot deliver: `180.0 * PI / 180.0` is
//! not π in binary, so its sine is ~1e-16, not 0. The fix is to reduce
//! the argument in its own unit (degrees, or half-revolutions) using
//! exact `fmod` and exact subtraction of the quadrant multiple, leaving
//! a small remainder for the radian libm routines — and to special-case
//! the remainders whose results are exactly representable.
//!
//! Reduction table (degrees; half-revolutions are the same with the
//! 90 → 0.5 and DEG_TO_RAD → PI substitutions):
//!
//!   r = x mod 360            exact fmod, r in [0, 360)
//!   n = round(r / 90)        nearest quadrant boundary, n in 0..=4
//!   rem = r - n*90           exact, |rem| <= 45
//!   q = n mod 4              quadrant
//!
//!   sin(r°) = [ sin, cos, -sin, -cos ][q] (rem°)
//!   cos(r°) = [ cos, -sin, -cos, sin ][q] (rem°)
//!   tan(r°) = q even: tan(rem°);  q odd: -cot(rem°)
//!
//! sin/tan are odd: the sign is split off first and reapplied so the
//! sign of zero survives (SIND(-180.0) is -0.0). cos is even.

use std::f64::consts::PI;

const DEG_TO_RAD: f64 = PI / 180.0;

unsafe extern "C" {
    #[link_name = "scalbn"]
    fn c_scalbn(x: f64, exponent: i32) -> f64;
    #[link_name = "scalbnf"]
    fn c_scalbnf(x: f32, exponent: i32) -> f32;
}

fn binary_exponent_f32(x: f32) -> i32 {
    let bits = x.to_bits() & 0x7fff_ffff;
    let exponent = (bits >> 23) & 0xff;
    if exponent != 0 {
        exponent as i32 - 127
    } else {
        let significand = bits & 0x007f_ffff;
        (31 - significand.leading_zeros()) as i32 - 149
    }
}

fn binary_exponent_f64(x: f64) -> i32 {
    let bits = x.to_bits() & 0x7fff_ffff_ffff_ffff;
    let exponent = (bits >> 52) & 0x7ff;
    if exponent != 0 {
        exponent as i32 - 1023
    } else {
        let significand = bits & 0x000f_ffff_ffff_ffff;
        (63 - significand.leading_zeros()) as i32 - 1074
    }
}

fn complex_div_f32(mut a: f32, mut b: f32, mut c: f32, mut d: f32) -> (f32, f32) {
    let numerator_scale = a.abs().max(b.abs());
    let divisor_scale = c.abs().max(d.abs());

    // Scaling both operands independently keeps all four products bounded.
    // Scaling only the divisor (the traditional Smith formulation) still
    // overflows for (MAX+i*MAX)/(MAX+i*MAX), even though the result is 1.
    if a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite() && divisor_scale != 0.0 {
        let numerator_exponent = if numerator_scale == 0.0 {
            0
        } else {
            binary_exponent_f32(numerator_scale)
        };
        let divisor_exponent = binary_exponent_f32(divisor_scale);
        unsafe {
            a = c_scalbnf(a, -numerator_exponent);
            b = c_scalbnf(b, -numerator_exponent);
            c = c_scalbnf(c, -divisor_exponent);
            d = c_scalbnf(d, -divisor_exponent);
        }
        let denominator = c * c + d * d;
        let real = (a * c + b * d) / denominator;
        let imaginary = (b * c - a * d) / denominator;
        let result_exponent = numerator_exponent - divisor_exponent;
        return unsafe {
            (
                c_scalbnf(real, result_exponent),
                c_scalbnf(imaginary, result_exponent),
            )
        };
    }

    // Match the C complex-arithmetic recovery rules for zero, infinite, and
    // NaN operands. Finite nonzero operands always take the safer path above.
    let mut divisor_exponent = 0;
    if divisor_scale.is_finite() && divisor_scale != 0.0 {
        divisor_exponent = binary_exponent_f32(divisor_scale);
        unsafe {
            c = c_scalbnf(c, -divisor_exponent);
            d = c_scalbnf(d, -divisor_exponent);
        }
    }
    let denominator = c * c + d * d;
    let mut real = unsafe { c_scalbnf((a * c + b * d) / denominator, -divisor_exponent) };
    let mut imaginary = unsafe { c_scalbnf((b * c - a * d) / denominator, -divisor_exponent) };
    if real.is_nan() && imaginary.is_nan() {
        if denominator == 0.0 && (!a.is_nan() || !b.is_nan()) {
            let infinity = f32::copysign(f32::INFINITY, c);
            real = infinity * a;
            imaginary = infinity * b;
        } else if (a.is_infinite() || b.is_infinite()) && c.is_finite() && d.is_finite() {
            a = f32::copysign(if a.is_infinite() { 1.0 } else { 0.0 }, a);
            b = f32::copysign(if b.is_infinite() { 1.0 } else { 0.0 }, b);
            real = f32::INFINITY * (a * c + b * d);
            imaginary = f32::INFINITY * (b * c - a * d);
        } else if divisor_scale.is_infinite() && a.is_finite() && b.is_finite() {
            c = f32::copysign(if c.is_infinite() { 1.0 } else { 0.0 }, c);
            d = f32::copysign(if d.is_infinite() { 1.0 } else { 0.0 }, d);
            real = 0.0 * (a * c + b * d);
            imaginary = 0.0 * (b * c - a * d);
        }
    }
    (real, imaginary)
}

fn complex_div_f64(mut a: f64, mut b: f64, mut c: f64, mut d: f64) -> (f64, f64) {
    let numerator_scale = a.abs().max(b.abs());
    let divisor_scale = c.abs().max(d.abs());

    if a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite() && divisor_scale != 0.0 {
        let numerator_exponent = if numerator_scale == 0.0 {
            0
        } else {
            binary_exponent_f64(numerator_scale)
        };
        let divisor_exponent = binary_exponent_f64(divisor_scale);
        unsafe {
            a = c_scalbn(a, -numerator_exponent);
            b = c_scalbn(b, -numerator_exponent);
            c = c_scalbn(c, -divisor_exponent);
            d = c_scalbn(d, -divisor_exponent);
        }
        let denominator = c * c + d * d;
        let real = (a * c + b * d) / denominator;
        let imaginary = (b * c - a * d) / denominator;
        let result_exponent = numerator_exponent - divisor_exponent;
        return unsafe {
            (
                c_scalbn(real, result_exponent),
                c_scalbn(imaginary, result_exponent),
            )
        };
    }

    let mut divisor_exponent = 0;
    if divisor_scale.is_finite() && divisor_scale != 0.0 {
        divisor_exponent = binary_exponent_f64(divisor_scale);
        unsafe {
            c = c_scalbn(c, -divisor_exponent);
            d = c_scalbn(d, -divisor_exponent);
        }
    }
    let denominator = c * c + d * d;
    let mut real = unsafe { c_scalbn((a * c + b * d) / denominator, -divisor_exponent) };
    let mut imaginary = unsafe { c_scalbn((b * c - a * d) / denominator, -divisor_exponent) };
    if real.is_nan() && imaginary.is_nan() {
        if denominator == 0.0 && (!a.is_nan() || !b.is_nan()) {
            let infinity = f64::copysign(f64::INFINITY, c);
            real = infinity * a;
            imaginary = infinity * b;
        } else if (a.is_infinite() || b.is_infinite()) && c.is_finite() && d.is_finite() {
            a = f64::copysign(if a.is_infinite() { 1.0 } else { 0.0 }, a);
            b = f64::copysign(if b.is_infinite() { 1.0 } else { 0.0 }, b);
            real = f64::INFINITY * (a * c + b * d);
            imaginary = f64::INFINITY * (b * c - a * d);
        } else if divisor_scale.is_infinite() && a.is_finite() && b.is_finite() {
            c = f64::copysign(if c.is_infinite() { 1.0 } else { 0.0 }, c);
            d = f64::copysign(if d.is_infinite() { 1.0 } else { 0.0 }, d);
            real = 0.0 * (a * c + b * d);
            imaginary = 0.0 * (b * c - a * d);
        }
    }
    (real, imaginary)
}

#[no_mangle]
pub extern "C" fn afs_complex_div_c4(
    result: *mut f32,
    lhs_real: f32,
    lhs_imaginary: f32,
    rhs_real: f32,
    rhs_imaginary: f32,
) {
    let (real, imaginary) = complex_div_f32(lhs_real, lhs_imaginary, rhs_real, rhs_imaginary);
    unsafe {
        result.write(real);
        result.add(1).write(imaginary);
    }
}

#[no_mangle]
pub extern "C" fn afs_complex_div_c8(
    result: *mut f64,
    lhs_real: f64,
    lhs_imaginary: f64,
    rhs_real: f64,
    rhs_imaginary: f64,
) {
    let (real, imaginary) = complex_div_f64(lhs_real, lhs_imaginary, rhs_real, rhs_imaginary);
    unsafe {
        result.write(real);
        result.add(1).write(imaginary);
    }
}

/// (quadrant in 0..=3, remainder in [-45, 45]) for x degrees, x >= 0.
fn reduce_deg(x: f64) -> (i64, f64) {
    let r = x % 360.0;
    let n = (r / 90.0).round();
    let rem = r - n * 90.0;
    (((n as i64) % 4 + 4) % 4, rem)
}

/// (quadrant in 0..=3, remainder in [-0.25, 0.25]) for x half-revs, x >= 0.
fn reduce_pi(x: f64) -> (i64, f64) {
    let r = x % 2.0;
    let n = (r / 0.5).round();
    let rem = r - n * 0.5;
    (((n as i64) % 4 + 4) % 4, rem)
}

// ---- degree forward trig ----

#[no_mangle]
pub extern "C" fn afs_sind(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let s = if x.is_sign_negative() { -1.0 } else { 1.0 };
    let (q, rem) = reduce_deg(x.abs());
    let v = if rem == 0.0 {
        match q {
            0 | 2 => 0.0,
            1 => 1.0,
            _ => -1.0,
        }
    } else {
        let rad = rem * DEG_TO_RAD;
        match q {
            0 => rad.sin(),
            1 => rad.cos(),
            2 => -rad.sin(),
            _ => -rad.cos(),
        }
    };
    s * v
}

#[no_mangle]
pub extern "C" fn afs_cosd(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (q, rem) = reduce_deg(x.abs());
    if rem == 0.0 {
        return match q {
            0 => 1.0,
            1 | 3 => 0.0,
            _ => -1.0,
        };
    }
    let rad = rem * DEG_TO_RAD;
    match q {
        0 => rad.cos(),
        1 => -rad.sin(),
        2 => -rad.cos(),
        _ => rad.sin(),
    }
}

#[no_mangle]
pub extern "C" fn afs_tand(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let s = if x.is_sign_negative() { -1.0 } else { 1.0 };
    let (q, rem) = reduce_deg(x.abs());
    let v = if rem == 0.0 {
        // q even: tan of 0°/180° = 0; q odd: pole at 90°/270°.
        match q {
            0 | 2 => 0.0,
            _ => f64::INFINITY,
        }
    } else if rem.abs() == 45.0 {
        // tan(±45°) = ±1 exactly; q odd applies -cot = -1/tan.
        let base = if rem > 0.0 { 1.0 } else { -1.0 };
        match q {
            0 | 2 => base,
            _ => -base,
        }
    } else {
        let rad = rem * DEG_TO_RAD;
        match q {
            0 | 2 => rad.tan(),
            _ => -1.0 / rad.tan(),
        }
    };
    s * v
}

// ---- degree inverse trig (result in degrees) ----

#[no_mangle]
pub extern "C" fn afs_asind(x: f64) -> f64 {
    if x == 0.0 {
        return x; // preserves sign of zero
    }
    if x == 1.0 {
        return 90.0;
    }
    if x == -1.0 {
        return -90.0;
    }
    x.asin().to_degrees()
}

#[no_mangle]
pub extern "C" fn afs_acosd(x: f64) -> f64 {
    if x == 1.0 {
        return 0.0;
    }
    if x == 0.0 {
        return 90.0;
    }
    if x == -1.0 {
        return 180.0;
    }
    x.acos().to_degrees()
}

#[no_mangle]
pub extern "C" fn afs_atand(x: f64) -> f64 {
    if x == 0.0 {
        return x;
    }
    if x == 1.0 {
        return 45.0;
    }
    if x == -1.0 {
        return -45.0;
    }
    x.atan().to_degrees()
}

#[no_mangle]
pub extern "C" fn afs_atan2d(y: f64, x: f64) -> f64 {
    // Cardinal directions are exact; copysign carries the sign of y so
    // ATAN2D(0.0,-1.0)=180 and ATAN2D(-0.0,-1.0)=-180.
    if y == 0.0 {
        if x > 0.0 {
            return f64::copysign(0.0, y);
        }
        if x < 0.0 {
            return f64::copysign(180.0, y);
        }
    }
    if x == 0.0 {
        if y > 0.0 {
            return 90.0;
        }
        if y < 0.0 {
            return -90.0;
        }
    }
    y.atan2(x).to_degrees()
}

// ---- half-revolution forward trig ----

#[no_mangle]
pub extern "C" fn afs_sinpi(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let s = if x.is_sign_negative() { -1.0 } else { 1.0 };
    let (q, rem) = reduce_pi(x.abs());
    let v = if rem == 0.0 {
        match q {
            0 | 2 => 0.0,
            1 => 1.0,
            _ => -1.0,
        }
    } else {
        let rad = rem * PI;
        match q {
            0 => rad.sin(),
            1 => rad.cos(),
            2 => -rad.sin(),
            _ => -rad.cos(),
        }
    };
    s * v
}

#[no_mangle]
pub extern "C" fn afs_cospi(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let (q, rem) = reduce_pi(x.abs());
    if rem == 0.0 {
        return match q {
            0 => 1.0,
            1 | 3 => 0.0,
            _ => -1.0,
        };
    }
    let rad = rem * PI;
    match q {
        0 => rad.cos(),
        1 => -rad.sin(),
        2 => -rad.cos(),
        _ => rad.sin(),
    }
}

#[no_mangle]
pub extern "C" fn afs_tanpi(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    let s = if x.is_sign_negative() { -1.0 } else { 1.0 };
    let (q, rem) = reduce_pi(x.abs());
    let v = if rem == 0.0 {
        match q {
            0 | 2 => 0.0,
            _ => f64::INFINITY,
        }
    } else if rem.abs() == 0.25 {
        let base = if rem > 0.0 { 1.0 } else { -1.0 };
        match q {
            0 | 2 => base,
            _ => -base,
        }
    } else {
        let rad = rem * PI;
        match q {
            0 | 2 => rad.tan(),
            _ => -1.0 / rad.tan(),
        }
    };
    s * v
}

// ---- half-revolution inverse trig (result in half-revolutions) ----

#[no_mangle]
pub extern "C" fn afs_asinpi(x: f64) -> f64 {
    if x == 0.0 {
        return x;
    }
    if x == 1.0 {
        return 0.5;
    }
    if x == -1.0 {
        return -0.5;
    }
    x.asin() / PI
}

#[no_mangle]
pub extern "C" fn afs_acospi(x: f64) -> f64 {
    if x == 1.0 {
        return 0.0;
    }
    if x == 0.0 {
        return 0.5;
    }
    if x == -1.0 {
        return 1.0;
    }
    x.acos() / PI
}

#[no_mangle]
pub extern "C" fn afs_atanpi(x: f64) -> f64 {
    if x == 0.0 {
        return x;
    }
    if x == 1.0 {
        return 0.25;
    }
    if x == -1.0 {
        return -0.25;
    }
    x.atan() / PI
}

#[no_mangle]
pub extern "C" fn afs_atan2pi(y: f64, x: f64) -> f64 {
    if y == 0.0 {
        if x > 0.0 {
            return f64::copysign(0.0, y);
        }
        if x < 0.0 {
            return f64::copysign(1.0, y);
        }
    }
    if x == 0.0 {
        if y > 0.0 {
            return 0.5;
        }
        if y < 0.0 {
            return -0.5;
        }
    }
    y.atan2(x) / PI
}

// ---- f32 variants ----
//
// Computed in f64 and narrowed: the f64 path's special cases land on
// exactly representable f32 values (0.0, ±1.0, 45.0, 90.0, 180.0),
// so SIND_F32(180.0) is 0.0f32 and TAND_F32(45.0) is 1.0f32 too, while
// the non-exact angles keep f64 working precision before the round.

macro_rules! f32_wrap1 {
    ($name:ident, $inner:ident) => {
        #[no_mangle]
        pub extern "C" fn $name(x: f32) -> f32 {
            $inner(x as f64) as f32
        }
    };
}
macro_rules! f32_wrap2 {
    ($name:ident, $inner:ident) => {
        #[no_mangle]
        pub extern "C" fn $name(y: f32, x: f32) -> f32 {
            $inner(y as f64, x as f64) as f32
        }
    };
}

f32_wrap1!(afs_sind_f32, afs_sind);
f32_wrap1!(afs_cosd_f32, afs_cosd);
f32_wrap1!(afs_tand_f32, afs_tand);
f32_wrap1!(afs_asind_f32, afs_asind);
f32_wrap1!(afs_acosd_f32, afs_acosd);
f32_wrap1!(afs_atand_f32, afs_atand);
f32_wrap2!(afs_atan2d_f32, afs_atan2d);
f32_wrap1!(afs_sinpi_f32, afs_sinpi);
f32_wrap1!(afs_cospi_f32, afs_cospi);
f32_wrap1!(afs_tanpi_f32, afs_tanpi);
f32_wrap1!(afs_asinpi_f32, afs_asinpi);
f32_wrap1!(afs_acospi_f32, afs_acospi);
f32_wrap1!(afs_atanpi_f32, afs_atanpi);
f32_wrap2!(afs_atan2pi_f32, afs_atan2pi);

/// SELECTED_LOGICAL_KIND(BITS) for non-constant arguments. The kind
/// set is fixed (1/2/4/8/16 occupy 8/16/32/64/128 bits); returns the
/// smallest kind with at least `bits` bits, or -1 if none. The compiler folds
/// constant arguments inline; this covers the runtime path.
#[no_mangle]
pub extern "C" fn afs_selected_logical_kind(bits: i32) -> i32 {
    if bits <= 8 {
        1
    } else if bits <= 16 {
        2
    } else if bits <= 32 {
        4
    } else if bits <= 64 {
        8
    } else if bits <= 128 {
        16
    } else {
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(x: f64) -> u64 {
        x.to_bits()
    }

    #[test]
    fn complex_division_scales_both_f32_operands() {
        for value in [2.0e30_f32, 2.0e-30_f32, f32::from_bits(1)] {
            let mut result = [0.0_f32; 2];
            afs_complex_div_c4(result.as_mut_ptr(), value, value, value, value);
            assert!((result[0] - 1.0).abs() <= 4.0 * f32::EPSILON, "{result:?}");
            assert!(result[1].abs() <= 4.0 * f32::EPSILON, "{result:?}");
        }
    }

    #[test]
    fn complex_division_scales_both_f64_operands() {
        for value in [2.0e300_f64, 2.0e-300_f64, f64::from_bits(1)] {
            let mut result = [0.0_f64; 2];
            afs_complex_div_c8(result.as_mut_ptr(), value, value, value, value);
            assert!((result[0] - 1.0).abs() <= 4.0 * f64::EPSILON, "{result:?}");
            assert!(result[1].abs() <= 4.0 * f64::EPSILON, "{result:?}");
        }
    }

    #[test]
    fn complex_division_exponent_bounds_are_exact() {
        assert_eq!(binary_exponent_f32(1.0), 0);
        assert_eq!(binary_exponent_f32(f32::MAX), 127);
        assert_eq!(binary_exponent_f32(f32::MIN_POSITIVE), -126);
        assert_eq!(binary_exponent_f32(f32::from_bits(1)), -149);
        assert_eq!(binary_exponent_f64(1.0), 0);
        assert_eq!(binary_exponent_f64(f64::MAX), 1023);
        assert_eq!(binary_exponent_f64(f64::MIN_POSITIVE), -1022);
        assert_eq!(binary_exponent_f64(f64::from_bits(1)), -1074);
    }

    #[test]
    fn complex_division_recovers_zero_and_infinite_divisors() {
        let mut single = [0.0_f32; 2];
        afs_complex_div_c4(single.as_mut_ptr(), 1.0, 2.0, 0.0, 0.0);
        assert!(single[0].is_infinite() && single[1].is_infinite());
        afs_complex_div_c4(single.as_mut_ptr(), 1.0, 2.0, f32::INFINITY, f32::INFINITY);
        assert_eq!(single, [0.0, 0.0]);

        let mut double = [0.0_f64; 2];
        afs_complex_div_c8(double.as_mut_ptr(), 1.0, 2.0, 0.0, 0.0);
        assert!(double[0].is_infinite() && double[1].is_infinite());
        afs_complex_div_c8(double.as_mut_ptr(), 1.0, 2.0, f64::INFINITY, f64::INFINITY);
        assert_eq!(double, [0.0, 0.0]);
    }

    #[test]
    fn degree_forward_exact_at_special_angles() {
        assert_eq!(afs_sind(0.0), 0.0);
        assert_eq!(afs_sind(90.0), 1.0);
        assert_eq!(afs_sind(180.0), 0.0);
        assert_eq!(afs_sind(270.0), -1.0);
        assert_eq!(afs_sind(360.0), 0.0);
        assert_eq!(afs_cosd(0.0), 1.0);
        assert_eq!(afs_cosd(90.0), 0.0);
        assert_eq!(afs_cosd(180.0), -1.0);
        assert_eq!(afs_tand(0.0), 0.0);
        assert_eq!(afs_tand(45.0), 1.0);
        assert_eq!(afs_tand(135.0), -1.0);
        assert_eq!(afs_tand(180.0), 0.0);
    }

    #[test]
    fn sign_of_zero_preserved() {
        // SIND is odd: SIND(-180) is -0.0, SIND(180) is +0.0.
        assert_eq!(bits(afs_sind(180.0)), bits(0.0));
        assert_eq!(bits(afs_sind(-180.0)), bits(-0.0));
        assert_eq!(bits(afs_sind(0.0)), bits(0.0));
        assert_eq!(bits(afs_sind(-0.0)), bits(-0.0));
    }

    #[test]
    fn degree_known_midpoints() {
        assert!((afs_sind(30.0) - 0.5).abs() < 1e-15);
        assert!((afs_cosd(60.0) - 0.5).abs() < 1e-15);
        assert!((afs_sind(45.0) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-15);
    }

    #[test]
    fn degree_inverse_exact() {
        assert_eq!(afs_asind(1.0), 90.0);
        assert_eq!(afs_asind(-1.0), -90.0);
        assert_eq!(afs_acosd(-1.0), 180.0);
        assert_eq!(afs_acosd(0.0), 90.0);
        assert_eq!(afs_atand(1.0), 45.0);
        assert_eq!(afs_atan2d(0.0, -1.0), 180.0);
        assert_eq!(afs_atan2d(1.0, 0.0), 90.0);
        assert_eq!(bits(afs_atan2d(-0.0, -1.0)), bits(-180.0));
    }

    #[test]
    fn halfrev_exact() {
        assert_eq!(afs_sinpi(1.0), 0.0);
        assert_eq!(afs_sinpi(0.5), 1.0);
        assert_eq!(afs_cospi(0.5), 0.0);
        assert_eq!(afs_cospi(0.0), 1.0);
        assert_eq!(afs_cospi(1.0), -1.0);
        assert_eq!(afs_tanpi(0.25), 1.0);
        assert_eq!(afs_atan2pi(0.0, -1.0), 1.0);
        assert_eq!(afs_acospi(-1.0), 1.0);
        assert_eq!(afs_asinpi(1.0), 0.5);
    }

    #[test]
    fn f32_variants_keep_exactness() {
        assert_eq!(afs_sind_f32(180.0), 0.0f32);
        assert_eq!(afs_tand_f32(45.0), 1.0f32);
        assert_eq!(afs_cosd_f32(90.0), 0.0f32);
        assert_eq!(afs_atan2d_f32(0.0, -1.0), 180.0f32);
        assert_eq!(afs_cospi_f32(0.5), 0.0f32);
    }

    #[test]
    fn nonfinite_is_nan() {
        assert!(afs_sind(f64::INFINITY).is_nan());
        assert!(afs_cosd(f64::NAN).is_nan());
        assert!(afs_tanpi(f64::NEG_INFINITY).is_nan());
    }
}
