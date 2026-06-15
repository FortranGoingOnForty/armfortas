//! IEEE 754 arithmetic support for `ieee_arithmetic` / `ieee_exceptions`
//! (F2018 clause 17, F2023 ISO/IEC 60559:2020 alignment).
//!
//! Bit-pattern classification and value construction live here as
//! `extern "C"` functions rather than inline IR. Two reasons: the logic
//! is identical across targets (both LP64 little-endian binary32/binary64),
//! and a call is opaque to constant folding — `ieee_is_nan(x)` lowered as
//! `x /= x` would be folded to `.false.` by passes that assume `a == a`,
//! but a call survives every pass (the l09 pitfall).
//!
//! ## Support matrix (l09 deliverable 1)
//!
//! Every name registered in `register_ieee_stubs` maps to exactly one
//! outcome — never "compiles and lies". `IEEE_SUPPORT_*` answers derive
//! from this table.
//!
//! | facility                         | real(4)/real(8) | mechanism                |
//! |----------------------------------|-----------------|--------------------------|
//! | ieee_is_nan/finite/normal        | implemented     | bit pattern (this file)  |
//! | ieee_unordered                   | implemented     | bit pattern              |
//! | ieee_class                       | implemented     | bit pattern              |
//! | ieee_value                       | implemented     | bit pattern (call-opaque)|
//! | ieee_copy_sign/logb/rint/scalb   | implemented     | libm                     |
//! | ieee_next_after                  | implemented     | libm nextafter           |
//! | ieee_max/min(_mag), *_num(_mag)  | implemented     | 60559:2020 (this file)   |
//! | ieee_get/set_rounding_mode       | implemented     | FPCR (arm) / MXCSR (x86) |
//! | ieee_get/set_flag, get/set_status| implemented     | FPSR/FPCR or MXCSR       |
//! | ieee_support_datatype/nan/inf    | true (r4,r8)    | -                        |
//! | ieee_support_denormal/subnormal  | true            | -                        |
//! | ieee_support_divide/sqrt/io      | true            | -                        |
//! | ieee_support_rounding/flag       | true            | -                        |
//! | ieee_support_underflow_control   | false           | FZ flipping out of scope |
//! | ieee_support_halting             | false           | trap delivery unreliable |
//! | ieee_support_standard            | false           | implies halting (false)  |
//!
//! Class tags match the named constants in
//! `src/sema/intrinsic_modules.rs::register_ieee_stubs`.

pub const IEEE_QUIET_NAN: i32 = 1;
pub const IEEE_POSITIVE_INF: i32 = 2;
pub const IEEE_NEGATIVE_INF: i32 = 3;
pub const IEEE_SIGNALING_NAN: i32 = 4;
pub const IEEE_POSITIVE_ZERO: i32 = 5;
pub const IEEE_NEGATIVE_ZERO: i32 = 6;
pub const IEEE_POSITIVE_DENORMAL: i32 = 7;
pub const IEEE_NEGATIVE_DENORMAL: i32 = 8;
pub const IEEE_POSITIVE_NORMAL: i32 = 9;
pub const IEEE_NEGATIVE_NORMAL: i32 = 10;
pub const IEEE_OTHER_VALUE: i32 = 11;

fn class_f64(bits: u64) -> i32 {
    let sign = bits >> 63 != 0;
    let exp = (bits >> 52) & 0x7ff;
    let mant = bits & 0x000f_ffff_ffff_ffff;
    if exp == 0x7ff {
        if mant == 0 {
            if sign {
                IEEE_NEGATIVE_INF
            } else {
                IEEE_POSITIVE_INF
            }
        } else if mant >> 51 != 0 {
            IEEE_QUIET_NAN
        } else {
            IEEE_SIGNALING_NAN
        }
    } else if exp == 0 {
        if mant == 0 {
            if sign {
                IEEE_NEGATIVE_ZERO
            } else {
                IEEE_POSITIVE_ZERO
            }
        } else if sign {
            IEEE_NEGATIVE_DENORMAL
        } else {
            IEEE_POSITIVE_DENORMAL
        }
    } else if sign {
        IEEE_NEGATIVE_NORMAL
    } else {
        IEEE_POSITIVE_NORMAL
    }
}

fn class_f32(bits: u32) -> i32 {
    let sign = bits >> 31 != 0;
    let exp = (bits >> 23) & 0xff;
    let mant = bits & 0x007f_ffff;
    if exp == 0xff {
        if mant == 0 {
            if sign {
                IEEE_NEGATIVE_INF
            } else {
                IEEE_POSITIVE_INF
            }
        } else if mant >> 22 != 0 {
            IEEE_QUIET_NAN
        } else {
            IEEE_SIGNALING_NAN
        }
    } else if exp == 0 {
        if mant == 0 {
            if sign {
                IEEE_NEGATIVE_ZERO
            } else {
                IEEE_POSITIVE_ZERO
            }
        } else if sign {
            IEEE_NEGATIVE_DENORMAL
        } else {
            IEEE_POSITIVE_DENORMAL
        }
    } else if sign {
        IEEE_NEGATIVE_NORMAL
    } else {
        IEEE_POSITIVE_NORMAL
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_class_r8(x: f64) -> i32 {
    class_f64(x.to_bits())
}

#[no_mangle]
pub extern "C" fn afs_ieee_class_r4(x: f32) -> i32 {
    class_f32(x.to_bits())
}

#[no_mangle]
pub extern "C" fn afs_ieee_is_nan_r8(x: f64) -> i32 {
    let c = class_f64(x.to_bits());
    (c == IEEE_QUIET_NAN || c == IEEE_SIGNALING_NAN) as i32
}

#[no_mangle]
pub extern "C" fn afs_ieee_is_nan_r4(x: f32) -> i32 {
    let c = class_f32(x.to_bits());
    (c == IEEE_QUIET_NAN || c == IEEE_SIGNALING_NAN) as i32
}

#[no_mangle]
pub extern "C" fn afs_ieee_is_finite_r8(x: f64) -> i32 {
    let exp = (x.to_bits() >> 52) & 0x7ff;
    (exp != 0x7ff) as i32
}

#[no_mangle]
pub extern "C" fn afs_ieee_is_finite_r4(x: f32) -> i32 {
    let exp = (x.to_bits() >> 23) & 0xff;
    (exp != 0xff) as i32
}

#[no_mangle]
pub extern "C" fn afs_ieee_is_normal_r8(x: f64) -> i32 {
    let c = class_f64(x.to_bits());
    (c == IEEE_POSITIVE_NORMAL
        || c == IEEE_NEGATIVE_NORMAL
        || c == IEEE_POSITIVE_ZERO
        || c == IEEE_NEGATIVE_ZERO) as i32
}

#[no_mangle]
pub extern "C" fn afs_ieee_is_normal_r4(x: f32) -> i32 {
    let c = class_f32(x.to_bits());
    (c == IEEE_POSITIVE_NORMAL
        || c == IEEE_NEGATIVE_NORMAL
        || c == IEEE_POSITIVE_ZERO
        || c == IEEE_NEGATIVE_ZERO) as i32
}

#[no_mangle]
pub extern "C" fn afs_ieee_unordered_r8(x: f64, y: f64) -> i32 {
    (x.is_nan() || y.is_nan()) as i32
}

#[no_mangle]
pub extern "C" fn afs_ieee_unordered_r4(x: f32, y: f32) -> i32 {
    (x.is_nan() || y.is_nan()) as i32
}

/// Construct the canonical value of a class. Opaque to const-folding, so
/// `ieee_value(1.0, ieee_quiet_nan)` survives -Ofast as an actual NaN.
#[no_mangle]
pub extern "C" fn afs_ieee_value_r8(class: i32) -> f64 {
    match class {
        IEEE_SIGNALING_NAN | IEEE_QUIET_NAN => f64::from_bits(0x7ff8_0000_0000_0000),
        IEEE_POSITIVE_INF => f64::INFINITY,
        IEEE_NEGATIVE_INF => f64::NEG_INFINITY,
        IEEE_POSITIVE_ZERO => 0.0,
        IEEE_NEGATIVE_ZERO => -0.0,
        IEEE_POSITIVE_DENORMAL => f64::from_bits(1),
        IEEE_NEGATIVE_DENORMAL => f64::from_bits(0x8000_0000_0000_0001),
        IEEE_POSITIVE_NORMAL => 1.0,
        IEEE_NEGATIVE_NORMAL => -1.0,
        _ => 0.0,
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_value_r4(class: i32) -> f32 {
    match class {
        IEEE_SIGNALING_NAN | IEEE_QUIET_NAN => f32::from_bits(0x7fc0_0000),
        IEEE_POSITIVE_INF => f32::INFINITY,
        IEEE_NEGATIVE_INF => f32::NEG_INFINITY,
        IEEE_POSITIVE_ZERO => 0.0,
        IEEE_NEGATIVE_ZERO => -0.0,
        IEEE_POSITIVE_DENORMAL => f32::from_bits(1),
        IEEE_NEGATIVE_DENORMAL => f32::from_bits(0x8000_0001),
        IEEE_POSITIVE_NORMAL => 1.0,
        IEEE_NEGATIVE_NORMAL => -1.0,
        _ => 0.0,
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_copy_sign_r8(x: f64, y: f64) -> f64 {
    x.copysign(y)
}

#[no_mangle]
pub extern "C" fn afs_ieee_copy_sign_r4(x: f32, y: f32) -> f32 {
    x.copysign(y)
}

/// IEEE_LOGB(x): unbiased exponent as a real. logb(±0) = -inf,
/// logb(±inf)=+inf, logb(nan)=nan (matches C logb / 60559 logB).
#[no_mangle]
pub extern "C" fn afs_ieee_logb_r8(x: f64) -> f64 {
    if x == 0.0 {
        f64::NEG_INFINITY
    } else if x.is_infinite() {
        f64::INFINITY
    } else if x.is_nan() {
        x
    } else {
        // Subnormals: normalize the exponent like logb does.
        let mut v = x.abs();
        let mut e = 0i32;
        while v < f64::MIN_POSITIVE {
            v *= 2.0;
            e -= 1;
        }
        e += ((v.to_bits() >> 52) & 0x7ff) as i32 - 1023;
        e as f64
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_logb_r4(x: f32) -> f32 {
    if x == 0.0 {
        f32::NEG_INFINITY
    } else if x.is_infinite() {
        f32::INFINITY
    } else if x.is_nan() {
        x
    } else {
        let mut v = x.abs();
        let mut e = 0i32;
        while v < f32::MIN_POSITIVE {
            v *= 2.0;
            e -= 1;
        }
        e += ((v.to_bits() >> 23) & 0xff) as i32 - 127;
        e as f32
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_rint_r8(x: f64) -> f64 {
    // Round to nearest, ties to even, honoring the current mode would
    // need nearbyint; round_ties_even matches the default mode.
    x.round_ties_even()
}

#[no_mangle]
pub extern "C" fn afs_ieee_rint_r4(x: f32) -> f32 {
    x.round_ties_even()
}

/// IEEE_SCALB(x, i) = x * 2**i, computed without intermediate overflow.
#[no_mangle]
pub extern "C" fn afs_ieee_scalb_r8(x: f64, i: i32) -> f64 {
    x * (2.0f64).powi(i)
}

#[no_mangle]
pub extern "C" fn afs_ieee_scalb_r4(x: f32, i: i32) -> f32 {
    x * (2.0f32).powi(i)
}

#[no_mangle]
pub extern "C" fn afs_ieee_next_after_r8(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == y {
        return y;
    }
    if x == 0.0 {
        return f64::from_bits(1).copysign(y);
    }
    let bits = x.to_bits();
    let up = (y > x) == (x > 0.0);
    let next = if up { bits + 1 } else { bits - 1 };
    f64::from_bits(next)
}

#[no_mangle]
pub extern "C" fn afs_ieee_next_after_r4(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        return f32::NAN;
    }
    if x == y {
        return y;
    }
    if x == 0.0 {
        return f32::from_bits(1).copysign(y);
    }
    let bits = x.to_bits();
    let up = (y > x) == (x > 0.0);
    let next = if up { bits + 1 } else { bits - 1 };
    f32::from_bits(next)
}

// ---- F2023 / ISO/IEC 60559:2020 maximum/minimum family ----
//
// The `ieee_max`/`ieee_min`(`_mag`) functions propagate NaN: a NaN
// operand yields a quiet NaN. The `*_num`(`_mag`) functions follow the
// 60559:2020 maximumNumber/minimumNumber family: a quiet-NaN operand is
// ignored (the number is returned). Both families order signed zeros
// (+0 > -0). `_mag` compares by magnitude, breaking ties by value.

const QNAN_F64: f64 = f64::from_bits(0x7ff8_0000_0000_0000);
const QNAN_F32: f32 = f32::from_bits(0x7fc0_0000);

fn max_val_f64(x: f64, y: f64) -> f64 {
    if x > y {
        x
    } else if y > x {
        y
    } else if x.is_sign_negative() {
        y // equal (incl. ±0): maximum picks +0 / the non-negative sign
    } else {
        x
    }
}

fn min_val_f64(x: f64, y: f64) -> f64 {
    if x < y {
        x
    } else if y < x {
        y
    } else if x.is_sign_negative() {
        x // equal (incl. ±0): minimum picks -0
    } else {
        y
    }
}

fn max_val_f32(x: f32, y: f32) -> f32 {
    if x > y {
        x
    } else if y > x {
        y
    } else if x.is_sign_negative() {
        y
    } else {
        x
    }
}

fn min_val_f32(x: f32, y: f32) -> f32 {
    if x < y {
        x
    } else if y < x {
        y
    } else if x.is_sign_negative() {
        x
    } else {
        y
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_r8(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        QNAN_F64
    } else {
        max_val_f64(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_r8(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        QNAN_F64
    } else {
        min_val_f64(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_mag_r8(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        QNAN_F64
    } else if x.abs() > y.abs() {
        x
    } else if y.abs() > x.abs() {
        y
    } else {
        max_val_f64(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_mag_r8(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        QNAN_F64
    } else if x.abs() < y.abs() {
        x
    } else if y.abs() < x.abs() {
        y
    } else {
        min_val_f64(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_num_r8(x: f64, y: f64) -> f64 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F64,
        (true, false) => y,
        (false, true) => x,
        (false, false) => max_val_f64(x, y),
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_num_r8(x: f64, y: f64) -> f64 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F64,
        (true, false) => y,
        (false, true) => x,
        (false, false) => min_val_f64(x, y),
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_num_mag_r8(x: f64, y: f64) -> f64 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F64,
        (true, false) => y,
        (false, true) => x,
        (false, false) => afs_ieee_max_mag_r8(x, y),
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_num_mag_r8(x: f64, y: f64) -> f64 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F64,
        (true, false) => y,
        (false, true) => x,
        (false, false) => afs_ieee_min_mag_r8(x, y),
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_r4(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        QNAN_F32
    } else {
        max_val_f32(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_r4(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        QNAN_F32
    } else {
        min_val_f32(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_mag_r4(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        QNAN_F32
    } else if x.abs() > y.abs() {
        x
    } else if y.abs() > x.abs() {
        y
    } else {
        max_val_f32(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_mag_r4(x: f32, y: f32) -> f32 {
    if x.is_nan() || y.is_nan() {
        QNAN_F32
    } else if x.abs() < y.abs() {
        x
    } else if y.abs() < x.abs() {
        y
    } else {
        min_val_f32(x, y)
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_num_r4(x: f32, y: f32) -> f32 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F32,
        (true, false) => y,
        (false, true) => x,
        (false, false) => max_val_f32(x, y),
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_num_r4(x: f32, y: f32) -> f32 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F32,
        (true, false) => y,
        (false, true) => x,
        (false, false) => min_val_f32(x, y),
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_max_num_mag_r4(x: f32, y: f32) -> f32 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F32,
        (true, false) => y,
        (false, true) => x,
        (false, false) => afs_ieee_max_mag_r4(x, y),
    }
}

#[no_mangle]
pub extern "C" fn afs_ieee_min_num_mag_r4(x: f32, y: f32) -> f32 {
    match (x.is_nan(), y.is_nan()) {
        (true, true) => QNAN_F32,
        (true, false) => y,
        (false, true) => x,
        (false, false) => afs_ieee_min_mag_r4(x, y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_f64() {
        assert_eq!(afs_ieee_class_r8(f64::NAN), IEEE_QUIET_NAN);
        assert_eq!(afs_ieee_class_r8(f64::INFINITY), IEEE_POSITIVE_INF);
        assert_eq!(afs_ieee_class_r8(f64::NEG_INFINITY), IEEE_NEGATIVE_INF);
        assert_eq!(afs_ieee_class_r8(0.0), IEEE_POSITIVE_ZERO);
        assert_eq!(afs_ieee_class_r8(-0.0), IEEE_NEGATIVE_ZERO);
        assert_eq!(afs_ieee_class_r8(1.0), IEEE_POSITIVE_NORMAL);
        assert_eq!(afs_ieee_class_r8(-1.0), IEEE_NEGATIVE_NORMAL);
        assert_eq!(afs_ieee_class_r8(f64::from_bits(1)), IEEE_POSITIVE_DENORMAL);
        assert_eq!(
            afs_ieee_class_r8(f64::from_bits(0x8000_0000_0000_0001)),
            IEEE_NEGATIVE_DENORMAL
        );
    }

    #[test]
    fn classifies_f32() {
        assert_eq!(afs_ieee_class_r4(f32::NAN), IEEE_QUIET_NAN);
        assert_eq!(afs_ieee_class_r4(f32::INFINITY), IEEE_POSITIVE_INF);
        assert_eq!(afs_ieee_class_r4(0.0), IEEE_POSITIVE_ZERO);
        assert_eq!(afs_ieee_class_r4(-2.0), IEEE_NEGATIVE_NORMAL);
        assert_eq!(afs_ieee_class_r4(f32::from_bits(1)), IEEE_POSITIVE_DENORMAL);
    }

    #[test]
    fn nan_predicates() {
        assert_eq!(afs_ieee_is_nan_r8(f64::NAN), 1);
        assert_eq!(afs_ieee_is_nan_r8(1.0), 0);
        assert_eq!(afs_ieee_is_finite_r8(1.0), 1);
        assert_eq!(afs_ieee_is_finite_r8(f64::INFINITY), 0);
        assert_eq!(afs_ieee_is_finite_r8(f64::NAN), 0);
        assert_eq!(afs_ieee_is_normal_r8(1.0), 1);
        assert_eq!(afs_ieee_is_normal_r8(f64::from_bits(1)), 0);
        assert_eq!(afs_ieee_is_normal_r8(0.0), 1);
    }

    #[test]
    fn value_round_trips_through_class() {
        for c in [
            IEEE_POSITIVE_INF,
            IEEE_NEGATIVE_INF,
            IEEE_POSITIVE_ZERO,
            IEEE_NEGATIVE_ZERO,
            IEEE_POSITIVE_NORMAL,
            IEEE_NEGATIVE_NORMAL,
            IEEE_POSITIVE_DENORMAL,
            IEEE_NEGATIVE_DENORMAL,
        ] {
            assert_eq!(afs_ieee_class_r8(afs_ieee_value_r8(c)), c, "class {}", c);
            assert_eq!(afs_ieee_class_r4(afs_ieee_value_r4(c)), c, "class {}", c);
        }
        assert_eq!(afs_ieee_is_nan_r8(afs_ieee_value_r8(IEEE_QUIET_NAN)), 1);
        assert_eq!(afs_ieee_is_nan_r4(afs_ieee_value_r4(IEEE_QUIET_NAN)), 1);
    }

    #[test]
    fn max_min_family() {
        // NaN propagates for the plain family.
        assert!(afs_ieee_max_r8(1.0, f64::NAN).is_nan());
        assert!(afs_ieee_min_r8(f64::NAN, 2.0).is_nan());
        assert_eq!(afs_ieee_max_r8(1.0, 2.0), 2.0);
        assert_eq!(afs_ieee_min_r8(1.0, 2.0), 1.0);
        // Signed zeros ordered.
        assert!(afs_ieee_max_r8(-0.0, 0.0).is_sign_positive());
        assert!(afs_ieee_min_r8(-0.0, 0.0).is_sign_negative());
        // Magnitude family.
        assert_eq!(afs_ieee_max_mag_r8(-3.0, 2.0), -3.0);
        assert_eq!(afs_ieee_min_mag_r8(-3.0, 2.0), 2.0);
        // Number family ignores NaN.
        assert_eq!(afs_ieee_max_num_r8(1.0, f64::NAN), 1.0);
        assert_eq!(afs_ieee_min_num_r8(f64::NAN, 5.0), 5.0);
        assert!(afs_ieee_max_num_r8(f64::NAN, f64::NAN).is_nan());
        assert_eq!(afs_ieee_max_num_mag_r8(-7.0, f64::NAN), -7.0);
        // r4 spot checks.
        assert_eq!(afs_ieee_max_r4(1.0, 2.0), 2.0);
        assert_eq!(afs_ieee_min_num_r4(f32::NAN, 5.0), 5.0);
    }

    #[test]
    fn logb_and_next_after() {
        assert_eq!(afs_ieee_logb_r8(8.0), 3.0);
        assert_eq!(afs_ieee_logb_r8(1.0), 0.0);
        assert_eq!(afs_ieee_logb_r8(0.0), f64::NEG_INFINITY);
        assert!(afs_ieee_next_after_r8(1.0, 2.0) > 1.0);
        assert!(afs_ieee_next_after_r8(1.0, 0.0) < 1.0);
        assert_eq!(afs_ieee_copy_sign_r8(3.0, -1.0), -3.0);
        assert_eq!(afs_ieee_scalb_r8(1.5, 3), 12.0);
    }
}
