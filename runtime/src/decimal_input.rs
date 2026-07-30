//! Exact decimal-to-binary conversion for formatted input.
//!
//! Rust's string parsers provide a correctly rounded nearest value.  Fortran
//! also requires directed input modes, and applying those modes after parsing
//! through a wider floating-point type can double-round.  This module keeps
//! the source value as an exact decimal, compares it with the nearest value's
//! exact finite decimal expansion, and moves by one target-width ULP when the
//! selected mode requires it.

use crate::format::RoundMode;
use std::cmp::Ordering;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExactDecimal {
    negative: bool,
    digits: String,
    exponent: i64,
}

impl ExactDecimal {
    fn parse(input: &str, implied_decimals: usize, scale_factor: i32) -> Option<Self> {
        let input = input.trim();
        let (negative, unsigned) = if let Some(rest) = input.strip_prefix('-') {
            (true, rest)
        } else if let Some(rest) = input.strip_prefix('+') {
            (false, rest)
        } else {
            (false, input)
        };

        let mut exponent_marker = None;
        for (index, byte) in unsigned.bytes().enumerate() {
            if matches!(byte, b'e' | b'E') && exponent_marker.replace(index).is_some() {
                return None;
            }
        }
        let (mantissa, explicit_exponent) = match exponent_marker {
            Some(index) => (
                &unsigned[..index],
                Some(parse_saturating_exponent(&unsigned[index + 1..])?),
            ),
            None => (unsigned, None),
        };

        let mut digits = String::with_capacity(mantissa.len());
        let mut saw_decimal = false;
        let mut fractional_digits = 0usize;
        for byte in mantissa.bytes() {
            match byte {
                b'0'..=b'9' => {
                    digits.push(char::from(byte));
                    if saw_decimal {
                        fractional_digits = fractional_digits.saturating_add(1);
                    }
                }
                b'.' if !saw_decimal => saw_decimal = true,
                _ => return None,
            }
        }
        if digits.is_empty() {
            return None;
        }

        let mut exponent = explicit_exponent.unwrap_or(0);
        let decimal_shift = if saw_decimal {
            saturating_usize_to_i64(fractional_digits)
        } else {
            saturating_usize_to_i64(implied_decimals)
        };
        exponent = exponent.saturating_sub(decimal_shift);
        if explicit_exponent.is_none() {
            exponent = exponent.saturating_sub(i64::from(scale_factor));
        }

        Some(Self::normalized(negative, digits, exponent))
    }

    fn from_binary_parts(negative: bool, significand: u128, exponent2: i32) -> Self {
        if significand == 0 {
            return Self::zero();
        }

        let mut integer = DecimalInteger::from(significand);
        let exponent = if exponent2 >= 0 {
            let mut remaining = exponent2 as u32;
            while remaining >= 29 {
                integer.multiply_small(1 << 29);
                remaining -= 29;
            }
            for _ in 0..remaining {
                integer.multiply_small(2);
            }
            0
        } else {
            let mut remaining = exponent2.unsigned_abs();
            while remaining >= 13 {
                integer.multiply_small(1_220_703_125); // 5^13
                remaining -= 13;
            }
            for _ in 0..remaining {
                integer.multiply_small(5);
            }
            i64::from(exponent2)
        };
        Self::normalized(negative, integer.to_decimal_string(), exponent)
    }

    fn normalized(negative: bool, mut digits: String, mut exponent: i64) -> Self {
        let first_nonzero = digits.bytes().position(|byte| byte != b'0');
        let Some(first_nonzero) = first_nonzero else {
            return Self {
                negative,
                digits: "0".to_string(),
                exponent: 0,
            };
        };
        if first_nonzero > 0 {
            digits.drain(..first_nonzero);
        }
        while digits.ends_with('0') {
            digits.pop();
            exponent = exponent.saturating_add(1);
        }
        Self {
            negative,
            digits,
            exponent,
        }
    }

    fn zero() -> Self {
        Self {
            negative: false,
            digits: "0".to_string(),
            exponent: 0,
        }
    }

    fn is_zero(&self) -> bool {
        self.digits == "0"
    }

    fn canonical_for_parser(&self) -> String {
        // Write scientific notation so a very long significand can cancel a
        // very negative decimal exponent without either quantity exceeding a
        // host parser's exponent syntax.  Scientific exponents outside this
        // range are already far beyond binary64's finite range.
        let scientific_exponent = i128::from(self.exponent) + self.digits.len() as i128 - 1;
        let scientific_exponent = scientific_exponent.clamp(-10_000, 10_000);
        let sign = if self.negative { "-" } else { "" };
        if self.digits.len() == 1 {
            format!("{sign}{}e{scientific_exponent}", self.digits)
        } else {
            format!(
                "{sign}{}.{}e{scientific_exponent}",
                &self.digits[..1],
                &self.digits[1..]
            )
        }
    }

    fn compare(&self, other: &Self) -> Ordering {
        if self.is_zero() && other.is_zero() {
            return Ordering::Equal;
        }
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }

        let magnitude = self.compare_magnitude(other);
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }

    fn compare_magnitude(&self, other: &Self) -> Ordering {
        if self.is_zero() {
            return if other.is_zero() {
                Ordering::Equal
            } else {
                Ordering::Less
            };
        }
        if other.is_zero() {
            return Ordering::Greater;
        }

        let self_order = self.digits.len() as i128 + i128::from(self.exponent);
        let other_order = other.digits.len() as i128 + i128::from(other.exponent);
        match self_order.cmp(&other_order) {
            Ordering::Equal => {}
            ordering => return ordering,
        }

        let self_digits = self.digits.as_bytes();
        let other_digits = other.digits.as_bytes();
        let width = self_digits.len().max(other_digits.len());
        for index in 0..width {
            let left = self_digits.get(index).copied().unwrap_or(b'0');
            let right = other_digits.get(index).copied().unwrap_or(b'0');
            match left.cmp(&right) {
                Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        Ordering::Equal
    }
}

fn parse_saturating_exponent(input: &str) -> Option<i64> {
    let (negative, digits) = if let Some(rest) = input.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = input.strip_prefix('+') {
        (false, rest)
    } else {
        (false, input)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let mut magnitude = 0i64;
    for byte in digits.bytes() {
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(i64::from(byte - b'0'));
    }
    Some(if negative {
        magnitude.saturating_neg()
    } else {
        magnitude
    })
}

fn saturating_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[derive(Debug)]
struct DecimalInteger {
    // Little-endian base 10^9 limbs.
    limbs: Vec<u32>,
}

impl From<u128> for DecimalInteger {
    fn from(mut value: u128) -> Self {
        const RADIX: u128 = 1_000_000_000;
        let mut limbs = Vec::new();
        while value != 0 {
            limbs.push((value % RADIX) as u32);
            value /= RADIX;
        }
        if limbs.is_empty() {
            limbs.push(0);
        }
        Self { limbs }
    }
}

impl DecimalInteger {
    fn multiply_small(&mut self, factor: u32) {
        const RADIX: u64 = 1_000_000_000;
        let mut carry = 0u64;
        for limb in &mut self.limbs {
            let product = u64::from(*limb) * u64::from(factor) + carry;
            *limb = (product % RADIX) as u32;
            carry = product / RADIX;
        }
        while carry != 0 {
            self.limbs.push((carry % RADIX) as u32);
            carry /= RADIX;
        }
    }

    fn to_decimal_string(&self) -> String {
        let mut limbs = self.limbs.iter().rev();
        let mut result = limbs
            .next()
            .expect("decimal integer has a limb")
            .to_string();
        for limb in limbs {
            result.push_str(&format!("{limb:09}"));
        }
        result
    }
}

#[derive(Clone, Copy)]
struct BinaryParts {
    negative: bool,
    significand: u64,
    exponent2: i32,
}

fn f32_parts(value: f32) -> BinaryParts {
    let bits = value.to_bits();
    let negative = bits >> 31 != 0;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let fraction = u64::from(bits & 0x7f_ffff);
    if exponent == 0 {
        BinaryParts {
            negative,
            significand: fraction,
            exponent2: -149,
        }
    } else {
        BinaryParts {
            negative,
            significand: (1u64 << 23) | fraction,
            exponent2: exponent - 127 - 23,
        }
    }
}

fn f64_parts(value: f64) -> BinaryParts {
    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    if exponent == 0 {
        BinaryParts {
            negative,
            significand: fraction,
            exponent2: -1074,
        }
    } else {
        BinaryParts {
            negative,
            significand: (1u64 << 52) | fraction,
            exponent2: exponent - 1023 - 52,
        }
    }
}

fn exact_from_parts(parts: BinaryParts) -> ExactDecimal {
    ExactDecimal::from_binary_parts(
        parts.negative,
        u128::from(parts.significand),
        parts.exponent2,
    )
}

fn exact_midpoint(left: BinaryParts, right: BinaryParts) -> ExactDecimal {
    let exponent2 = left.exponent2.min(right.exponent2);
    let left_shift = u32::try_from(left.exponent2 - exponent2).expect("nonnegative shift");
    let right_shift = u32::try_from(right.exponent2 - exponent2).expect("nonnegative shift");
    let significand = (u128::from(left.significand) << left_shift)
        + (u128::from(right.significand) << right_shift);
    ExactDecimal::from_binary_parts(left.negative, significand, exponent2 - 1)
}

fn next_up_f32(value: f32) -> f32 {
    if value.is_nan() || value == f32::INFINITY {
        return value;
    }
    if value == 0.0 {
        return f32::from_bits(1);
    }
    let bits = value.to_bits();
    f32::from_bits(if value.is_sign_negative() {
        bits - 1
    } else {
        bits + 1
    })
}

fn next_down_f32(value: f32) -> f32 {
    if value.is_nan() || value == f32::NEG_INFINITY {
        return value;
    }
    if value == 0.0 {
        return f32::from_bits(0x8000_0001);
    }
    let bits = value.to_bits();
    f32::from_bits(if value.is_sign_negative() {
        bits + 1
    } else {
        bits - 1
    })
}

fn next_up_f64(value: f64) -> f64 {
    if value.is_nan() || value == f64::INFINITY {
        return value;
    }
    if value == 0.0 {
        return f64::from_bits(1);
    }
    let bits = value.to_bits();
    f64::from_bits(if value.is_sign_negative() {
        bits - 1
    } else {
        bits + 1
    })
}

fn next_down_f64(value: f64) -> f64 {
    if value.is_nan() || value == f64::NEG_INFINITY {
        return value;
    }
    if value == 0.0 {
        return f64::from_bits(0x8000_0000_0000_0001);
    }
    let bits = value.to_bits();
    f64::from_bits(if value.is_sign_negative() {
        bits + 1
    } else {
        bits - 1
    })
}

fn adjust_f32(exact: &ExactDecimal, nearest: f32, mode: RoundMode) -> f32 {
    if nearest.is_infinite() {
        return match mode {
            RoundMode::Up if exact.negative => -f32::MAX,
            RoundMode::Down if !exact.negative => f32::MAX,
            RoundMode::Zero => {
                if exact.negative {
                    -f32::MAX
                } else {
                    f32::MAX
                }
            }
            _ => nearest,
        };
    }

    let relation = exact.compare(&exact_from_parts(f32_parts(nearest)));
    if relation == Ordering::Equal {
        return nearest;
    }
    match mode {
        RoundMode::Up => {
            if relation == Ordering::Greater {
                next_up_f32(nearest)
            } else {
                nearest
            }
        }
        RoundMode::Down => {
            if relation == Ordering::Less {
                next_down_f32(nearest)
            } else {
                nearest
            }
        }
        RoundMode::Zero => {
            if exact.negative {
                if relation == Ordering::Greater {
                    next_up_f32(nearest)
                } else {
                    nearest
                }
            } else if relation == Ordering::Less {
                next_down_f32(nearest)
            } else {
                nearest
            }
        }
        RoundMode::Compatible => {
            let away = if exact.negative {
                next_down_f32(nearest)
            } else {
                next_up_f32(nearest)
            };
            if away.is_finite()
                && exact.compare(&exact_midpoint(f32_parts(nearest), f32_parts(away)))
                    == Ordering::Equal
            {
                away
            } else {
                nearest
            }
        }
        RoundMode::Nearest | RoundMode::ProcessorDefined => nearest,
    }
}

fn adjust_f64(exact: &ExactDecimal, nearest: f64, mode: RoundMode) -> f64 {
    if nearest.is_infinite() {
        return match mode {
            RoundMode::Up if exact.negative => -f64::MAX,
            RoundMode::Down if !exact.negative => f64::MAX,
            RoundMode::Zero => {
                if exact.negative {
                    -f64::MAX
                } else {
                    f64::MAX
                }
            }
            _ => nearest,
        };
    }

    let relation = exact.compare(&exact_from_parts(f64_parts(nearest)));
    if relation == Ordering::Equal {
        return nearest;
    }
    match mode {
        RoundMode::Up => {
            if relation == Ordering::Greater {
                next_up_f64(nearest)
            } else {
                nearest
            }
        }
        RoundMode::Down => {
            if relation == Ordering::Less {
                next_down_f64(nearest)
            } else {
                nearest
            }
        }
        RoundMode::Zero => {
            if exact.negative {
                if relation == Ordering::Greater {
                    next_up_f64(nearest)
                } else {
                    nearest
                }
            } else if relation == Ordering::Less {
                next_down_f64(nearest)
            } else {
                nearest
            }
        }
        RoundMode::Compatible => {
            let away = if exact.negative {
                next_down_f64(nearest)
            } else {
                next_up_f64(nearest)
            };
            if away.is_finite()
                && exact.compare(&exact_midpoint(f64_parts(nearest), f64_parts(away)))
                    == Ordering::Equal
            {
                away
            } else {
                nearest
            }
        }
        RoundMode::Nearest | RoundMode::ProcessorDefined => nearest,
    }
}

fn has_special_spelling(input: &str) -> bool {
    input
        .bytes()
        .any(|byte| byte.is_ascii_alphabetic() && !matches!(byte, b'e' | b'E'))
}

pub(crate) fn parse_f32(
    input: &str,
    implied_decimals: usize,
    scale_factor: i32,
    mode: RoundMode,
) -> Option<f32> {
    if has_special_spelling(input) {
        return input.trim().parse::<f32>().ok();
    }
    let exact = ExactDecimal::parse(input, implied_decimals, scale_factor)?;
    let nearest = exact.canonical_for_parser().parse::<f32>().ok()?;
    Some(adjust_f32(&exact, nearest, mode))
}

pub(crate) fn parse_f64(
    input: &str,
    implied_decimals: usize,
    scale_factor: i32,
    mode: RoundMode,
) -> Option<f64> {
    if has_special_spelling(input) {
        return input.trim().parse::<f64>().ok();
    }
    let exact = ExactDecimal::parse(input, implied_decimals, scale_factor)?;
    let nearest = exact.canonical_for_parser().parse::<f64>().ok()?;
    Some(adjust_f64(&exact, nearest, mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSITIVE32: &str = "1.000000059604644775390625";
    const NEGATIVE32: &str = "-1.000000059604644775390625";
    const POSITIVE64: &str = "1.00000000000000011102230246251565404236316680908203125";
    const NEGATIVE64: &str = "-1.00000000000000011102230246251565404236316680908203125";

    #[test]
    fn directed_rounding_selects_target_width_neighbors() {
        assert_eq!(
            parse_f32(POSITIVE32, 24, 0, RoundMode::Up)
                .unwrap()
                .to_bits(),
            0x3f80_0001
        );
        assert_eq!(
            parse_f32(POSITIVE32, 24, 0, RoundMode::Down)
                .unwrap()
                .to_bits(),
            0x3f80_0000
        );
        assert_eq!(
            parse_f32(NEGATIVE32, 24, 0, RoundMode::Up)
                .unwrap()
                .to_bits(),
            0xbf80_0000
        );
        assert_eq!(
            parse_f32(NEGATIVE32, 24, 0, RoundMode::Down)
                .unwrap()
                .to_bits(),
            0xbf80_0001
        );
        assert_eq!(
            parse_f64(POSITIVE64, 53, 0, RoundMode::Up)
                .unwrap()
                .to_bits(),
            0x3ff0_0000_0000_0001
        );
        assert_eq!(
            parse_f64(POSITIVE64, 53, 0, RoundMode::Down)
                .unwrap()
                .to_bits(),
            0x3ff0_0000_0000_0000
        );
        assert_eq!(
            parse_f64(NEGATIVE64, 53, 0, RoundMode::Up)
                .unwrap()
                .to_bits(),
            0xbff0_0000_0000_0000
        );
        assert_eq!(
            parse_f64(NEGATIVE64, 53, 0, RoundMode::Down)
                .unwrap()
                .to_bits(),
            0xbff0_0000_0000_0001
        );
    }

    #[test]
    fn compatible_breaks_ties_away_and_nearest_breaks_ties_even() {
        assert_eq!(
            parse_f32(POSITIVE32, 24, 0, RoundMode::Compatible)
                .unwrap()
                .to_bits(),
            0x3f80_0001
        );
        assert_eq!(
            parse_f32(NEGATIVE32, 24, 0, RoundMode::Compatible)
                .unwrap()
                .to_bits(),
            0xbf80_0001
        );
        assert_eq!(
            parse_f32(POSITIVE32, 24, 0, RoundMode::Nearest)
                .unwrap()
                .to_bits(),
            0x3f80_0000
        );
    }

    #[test]
    fn directed_overflow_and_underflow_keep_the_inward_neighbor() {
        assert_eq!(parse_f32("1e10000", 0, 0, RoundMode::Down), Some(f32::MAX));
        assert_eq!(parse_f32("-1e10000", 0, 0, RoundMode::Up), Some(-f32::MAX));
        assert_eq!(
            parse_f32("1e10000", 0, 0, RoundMode::Up),
            Some(f32::INFINITY)
        );
        assert_eq!(
            parse_f32("-1e10000", 0, 0, RoundMode::Down),
            Some(f32::NEG_INFINITY)
        );
        assert_eq!(
            parse_f32("1e-10000", 0, 0, RoundMode::Up)
                .unwrap()
                .to_bits(),
            1
        );
        assert_eq!(
            parse_f32("-1e-10000", 0, 0, RoundMode::Down)
                .unwrap()
                .to_bits(),
            0x8000_0001
        );
        assert_eq!(
            parse_f32("1e-10000", 0, 0, RoundMode::Down)
                .unwrap()
                .to_bits(),
            0
        );
        assert_eq!(
            parse_f32("-1e-10000", 0, 0, RoundMode::Up)
                .unwrap()
                .to_bits(),
            0x8000_0000
        );
    }

    #[test]
    fn round_to_zero_moves_inward_for_both_signs() {
        assert_eq!(
            parse_f64(POSITIVE64, 53, 0, RoundMode::Zero)
                .unwrap()
                .to_bits(),
            0x3ff0_0000_0000_0000
        );
        assert_eq!(
            parse_f64(NEGATIVE64, 53, 0, RoundMode::Zero)
                .unwrap()
                .to_bits(),
            0xbff0_0000_0000_0000
        );
    }

    #[test]
    fn implied_decimal_and_scale_are_applied_before_rounding() {
        assert_eq!(
            parse_f32("1000000059604644775390625", 24, 0, RoundMode::Up)
                .unwrap()
                .to_bits(),
            0x3f80_0001
        );
        assert_eq!(
            parse_f32("10.00000059604644775390625", 24, 1, RoundMode::Down)
                .unwrap()
                .to_bits(),
            0x3f80_0000
        );
        assert_eq!(parse_f64("1.25", 2, 0, RoundMode::Up), Some(1.25));
    }

    #[test]
    fn long_significand_and_exponent_cancel_without_misclassification() {
        let input = format!("1{}1e-20000", "0".repeat(19_999));
        assert_eq!(
            parse_f64(&input, 0, 0, RoundMode::Up).unwrap().to_bits(),
            0x3ff0_0000_0000_0001
        );
        assert_eq!(
            parse_f64(&input, 0, 0, RoundMode::Down).unwrap().to_bits(),
            0x3ff0_0000_0000_0000
        );
    }

    #[test]
    fn finite_binary_expansions_round_trip_exactly() {
        let f32_cases = [
            0x0000_0001,
            0x007f_ffff,
            0x0080_0000,
            0x3f7f_ffff,
            0x3f80_0000,
            0x3f80_0001,
            0x7f7f_ffff,
            0x8000_0001,
            0xbf80_0001,
            0xff7f_ffff,
        ];
        for bits in f32_cases {
            let value = f32::from_bits(bits);
            let exact = exact_from_parts(f32_parts(value));
            let reparsed = exact.canonical_for_parser().parse::<f32>().unwrap();
            assert_eq!(reparsed.to_bits(), bits, "f32 bits {bits:#010x}");
        }

        let f64_cases = [
            0x0000_0000_0000_0001,
            0x000f_ffff_ffff_ffff,
            0x0010_0000_0000_0000,
            0x3fef_ffff_ffff_ffff,
            0x3ff0_0000_0000_0000,
            0x3ff0_0000_0000_0001,
            0x7fef_ffff_ffff_ffff,
            0x8000_0000_0000_0001,
            0xbff0_0000_0000_0001,
            0xffef_ffff_ffff_ffff,
        ];
        for bits in f64_cases {
            let value = f64::from_bits(bits);
            let exact = exact_from_parts(f64_parts(value));
            let reparsed = exact.canonical_for_parser().parse::<f64>().unwrap();
            assert_eq!(reparsed.to_bits(), bits, "f64 bits {bits:#018x}");
        }
    }
}
