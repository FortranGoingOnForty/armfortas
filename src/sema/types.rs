//! Fortran type system.
//!
//! Type representation, arithmetic promotion, implicit conversions,
//! and expression type checking.

/// A Fortran type.
#[derive(Debug, Clone, PartialEq)]
pub enum FortranType {
    Integer { kind: u8 },        // kind in bytes: 1, 2, 4, 8
    Real { kind: u8 },           // 4 (single), 8 (double), 16 (quad)
    Complex { kind: u8 },        // 4, 8, 16
    Logical { kind: u8 },        // 1, 2, 4, 8
    Character { kind: u8, len: CharLen },
    Derived { name: String },
    ClassOf { base: String },    // CLASS(t)
    UnlimitedPoly,               // CLASS(*)
    AssumedType,                 // TYPE(*)
    Void,                        // subroutine (no return value)
    Unknown,                     // not yet determined
}

/// Character length.
#[derive(Debug, Clone, PartialEq)]
pub enum CharLen {
    Known(i64),
    Assumed,    // len=*
    Deferred,   // len=:
    Unknown,    // runtime expression
}

impl FortranType {
    /// Default integer: integer(4).
    pub fn default_integer() -> Self { Self::Integer { kind: 4 } }
    /// Default real: real(4).
    pub fn default_real() -> Self { Self::Real { kind: 4 } }
    /// Default double precision: real(8).
    pub fn double_precision() -> Self { Self::Real { kind: 8 } }
    /// Default complex: complex(4).
    pub fn default_complex() -> Self { Self::Complex { kind: 4 } }
    /// Default logical: logical(4).
    pub fn default_logical() -> Self { Self::Logical { kind: 4 } }
    /// Default character: character(1, len=1).
    pub fn default_character() -> Self { Self::Character { kind: 1, len: CharLen::Known(1) } }

    /// Is this a numeric type (integer, real, or complex)?
    pub fn is_numeric(&self) -> bool {
        matches!(self, Self::Integer { .. } | Self::Real { .. } | Self::Complex { .. })
    }

    /// Is this a logical type?
    pub fn is_logical(&self) -> bool { matches!(self, Self::Logical { .. }) }

    /// Is this a character type?
    pub fn is_character(&self) -> bool { matches!(self, Self::Character { .. }) }

    /// Get the kind (size in bytes) for numeric/logical types.
    pub fn kind(&self) -> Option<u8> {
        match self {
            Self::Integer { kind } | Self::Real { kind } |
            Self::Complex { kind } | Self::Logical { kind } |
            Self::Character { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// Numeric rank for promotion: integer < real < complex.
    fn numeric_rank(&self) -> u8 {
        match self {
            Self::Integer { .. } => 1,
            Self::Real { .. } => 2,
            Self::Complex { .. } => 3,
            _ => 0,
        }
    }
}

/// Compute the result type of a binary arithmetic operation.
/// Implements Fortran's type promotion rules.
pub fn arithmetic_result_type(left: &FortranType, right: &FortranType) -> Option<FortranType> {
    if !left.is_numeric() || !right.is_numeric() {
        return None;
    }

    let left_rank = left.numeric_rank();
    let right_rank = right.numeric_rank();
    let left_kind = left.kind().unwrap_or(4);
    let right_kind = right.kind().unwrap_or(4);

    // Promote to the wider type.
    let result_rank = left_rank.max(right_rank);
    let result_kind = if left_rank == right_rank {
        left_kind.max(right_kind) // same type class → larger kind
    } else if result_rank == left_rank {
        left_kind
    } else {
        right_kind
    };

    Some(match result_rank {
        1 => FortranType::Integer { kind: result_kind },
        2 => FortranType::Real { kind: result_kind },
        3 => FortranType::Complex { kind: result_kind },
        _ => return None,
    })
}

/// Compute the result type of a power operation.
/// integer ** integer → integer; real ** integer → real (special case).
pub fn power_result_type(base: &FortranType, exponent: &FortranType) -> Option<FortranType> {
    if !base.is_numeric() || !exponent.is_numeric() {
        return None;
    }
    // If both integer, result is integer.
    if matches!(base, FortranType::Integer { .. }) && matches!(exponent, FortranType::Integer { .. }) {
        let kind = base.kind().unwrap_or(4).max(exponent.kind().unwrap_or(4));
        return Some(FortranType::Integer { kind });
    }
    // Otherwise, promote base to at least real.
    let promoted_base = if matches!(base, FortranType::Integer { .. }) {
        FortranType::Real { kind: base.kind().unwrap_or(4) }
    } else {
        base.clone()
    };
    Some(promoted_base)
}

/// Comparison operators always produce logical.
pub fn comparison_result_type() -> FortranType {
    FortranType::default_logical()
}

/// Concatenation produces character with combined length.
pub fn concat_result_type(left: &FortranType, right: &FortranType) -> Option<FortranType> {
    if !left.is_character() || !right.is_character() {
        return None;
    }
    let left_len = if let FortranType::Character { len, .. } = left { len } else { return None; };
    let right_len = if let FortranType::Character { len, .. } = right { len } else { return None; };

    let result_len = match (left_len, right_len) {
        (CharLen::Known(a), CharLen::Known(b)) => CharLen::Known(a + b),
        _ => CharLen::Unknown,
    };
    Some(FortranType::Character { kind: 1, len: result_len })
}

/// Check if an implicit conversion is needed from `from` to `to`.
/// Returns None if no conversion needed, or the target type if needed.
pub fn needs_conversion(from: &FortranType, to: &FortranType) -> Option<FortranType> {
    if from == to { return None; }
    if from.is_numeric() && to.is_numeric() {
        // Numeric → numeric conversion always possible.
        return Some(to.clone());
    }
    None
}

/// Get the type of a common intrinsic function call.
pub fn intrinsic_result_type(name: &str, args: &[FortranType]) -> Option<FortranType> {
    let lower = name.to_lowercase();
    match lower.as_str() {
        // Type-preserving: result has same type as argument.
        "abs" => {
            match args.first()? {
                FortranType::Integer { kind } => Some(FortranType::Integer { kind: *kind }),
                FortranType::Real { kind } => Some(FortranType::Real { kind: *kind }),
                FortranType::Complex { kind } => Some(FortranType::Real { kind: *kind }), // abs(complex) → real
                _ => None,
            }
        }
        "max" | "min" => args.first().cloned(),
        "sign" | "mod" | "modulo" => args.first().cloned(),

        // Real-valued math.
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" |
        "exp" | "log" | "log10" | "sqrt" | "atan2" => {
            Some(args.first().cloned().unwrap_or(FortranType::default_real()))
        }

        // Integer-valued.
        "int" | "nint" | "floor" | "ceiling" => Some(FortranType::default_integer()),
        "len" | "len_trim" | "index" | "scan" | "verify" => Some(FortranType::default_integer()),
        "size" | "lbound" | "ubound" | "shape" => Some(FortranType::default_integer()),
        "kind" | "selected_int_kind" | "selected_real_kind" => Some(FortranType::default_integer()),
        "iand" | "ior" | "ieor" | "ishft" | "ibits" => args.first().cloned(),
        "bit_size" | "leadz" | "trailz" | "popcount" => Some(FortranType::default_integer()),

        // Real-valued.
        "real" | "float" | "dble" | "dfloat" => Some(FortranType::default_real()),
        "aimag" | "conjg" => args.first().cloned(),

        // Logical-valued.
        "allocated" | "associated" | "present" | "btest" => Some(FortranType::default_logical()),
        "lge" | "lgt" | "lle" | "llt" => Some(FortranType::default_logical()),
        "any" | "all" => Some(FortranType::default_logical()),

        // Character-valued.
        "trim" | "adjustl" | "adjustr" | "repeat" => {
            Some(FortranType::Character { kind: 1, len: CharLen::Unknown })
        }
        "char" | "achar" => Some(FortranType::Character { kind: 1, len: CharLen::Known(1) }),

        // Complex-valued.
        "cmplx" => Some(FortranType::default_complex()),

        _ => None, // Unknown intrinsic.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Arithmetic promotion ----

    #[test]
    fn int_plus_int() {
        let result = arithmetic_result_type(
            &FortranType::Integer { kind: 4 },
            &FortranType::Integer { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Integer { kind: 4 });
    }

    #[test]
    fn int_plus_real() {
        let result = arithmetic_result_type(
            &FortranType::Integer { kind: 4 },
            &FortranType::Real { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Real { kind: 4 });
    }

    #[test]
    fn real_plus_complex() {
        let result = arithmetic_result_type(
            &FortranType::Real { kind: 4 },
            &FortranType::Complex { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Complex { kind: 4 });
    }

    #[test]
    fn int4_plus_real8() {
        let result = arithmetic_result_type(
            &FortranType::Integer { kind: 4 },
            &FortranType::Real { kind: 8 },
        ).unwrap();
        assert_eq!(result, FortranType::Real { kind: 8 });
    }

    #[test]
    fn int4_plus_int8() {
        let result = arithmetic_result_type(
            &FortranType::Integer { kind: 4 },
            &FortranType::Integer { kind: 8 },
        ).unwrap();
        assert_eq!(result, FortranType::Integer { kind: 8 });
    }

    #[test]
    fn non_numeric_arithmetic_returns_none() {
        assert!(arithmetic_result_type(
            &FortranType::default_logical(),
            &FortranType::default_integer(),
        ).is_none());
    }

    // ---- Power ----

    #[test]
    fn int_power_int() {
        let result = power_result_type(
            &FortranType::Integer { kind: 4 },
            &FortranType::Integer { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Integer { kind: 4 });
    }

    #[test]
    fn real_power_int() {
        let result = power_result_type(
            &FortranType::Real { kind: 8 },
            &FortranType::Integer { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Real { kind: 8 });
    }

    // ---- Comparison ----

    #[test]
    fn comparison_is_logical() {
        assert_eq!(comparison_result_type(), FortranType::default_logical());
    }

    // ---- Concatenation ----

    #[test]
    fn concat_known_lengths() {
        let result = concat_result_type(
            &FortranType::Character { kind: 1, len: CharLen::Known(5) },
            &FortranType::Character { kind: 1, len: CharLen::Known(3) },
        ).unwrap();
        assert_eq!(result, FortranType::Character { kind: 1, len: CharLen::Known(8) });
    }

    #[test]
    fn concat_non_character_returns_none() {
        assert!(concat_result_type(
            &FortranType::default_integer(),
            &FortranType::Character { kind: 1, len: CharLen::Known(5) },
        ).is_none());
    }

    // ---- Conversion ----

    #[test]
    fn no_conversion_same_type() {
        assert!(needs_conversion(
            &FortranType::Integer { kind: 4 },
            &FortranType::Integer { kind: 4 },
        ).is_none());
    }

    #[test]
    fn int_to_real_conversion() {
        let conv = needs_conversion(
            &FortranType::Integer { kind: 4 },
            &FortranType::Real { kind: 4 },
        ).unwrap();
        assert_eq!(conv, FortranType::Real { kind: 4 });
    }

    // ---- Intrinsics ----

    #[test]
    fn abs_integer() {
        let result = intrinsic_result_type("abs", &[FortranType::Integer { kind: 4 }]).unwrap();
        assert_eq!(result, FortranType::Integer { kind: 4 });
    }

    #[test]
    fn abs_complex_gives_real() {
        let result = intrinsic_result_type("abs", &[FortranType::Complex { kind: 8 }]).unwrap();
        assert_eq!(result, FortranType::Real { kind: 8 });
    }

    #[test]
    fn sin_returns_real() {
        let result = intrinsic_result_type("sin", &[FortranType::Real { kind: 8 }]).unwrap();
        assert_eq!(result, FortranType::Real { kind: 8 });
    }

    #[test]
    fn len_returns_integer() {
        let result = intrinsic_result_type("len", &[
            FortranType::Character { kind: 1, len: CharLen::Known(10) }
        ]).unwrap();
        assert_eq!(result, FortranType::default_integer());
    }

    #[test]
    fn allocated_returns_logical() {
        let result = intrinsic_result_type("allocated", &[FortranType::default_integer()]).unwrap();
        assert_eq!(result, FortranType::default_logical());
    }

    #[test]
    fn trim_returns_character() {
        let result = intrinsic_result_type("trim", &[
            FortranType::Character { kind: 1, len: CharLen::Known(20) }
        ]).unwrap();
        assert!(result.is_character());
    }

    #[test]
    fn unknown_intrinsic_returns_none() {
        assert!(intrinsic_result_type("nonexistent_func", &[FortranType::default_integer()]).is_none());
    }
}
