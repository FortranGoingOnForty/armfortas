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

/// Array type information — wraps an element type with rank and shape.
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayType {
    pub element_type: FortranType,
    pub rank: u8,
    pub shape: ArrayShape,
}

/// Shape of an array.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrayShape {
    Explicit(Vec<Dimension>),
    AssumedShape(u8),
    AssumedSize,
    Deferred(u8),
    AssumedRank,
}

/// A single array dimension.
#[derive(Debug, Clone, PartialEq)]
pub struct Dimension {
    pub lower: Bound,
    pub upper: Bound,
}

/// An array bound — known at compile time or determined at runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum Bound {
    Constant(i64),
    Runtime,
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
    // Otherwise, promote base to at least real, then apply normal promotion with exponent.
    let promoted_base = if matches!(base, FortranType::Integer { .. }) {
        FortranType::Real { kind: base.kind().unwrap_or(4) }
    } else {
        base.clone()
    };
    arithmetic_result_type(&promoted_base, exponent).or(Some(promoted_base))
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

/// Logical operators require logical operands and produce logical.
pub fn logical_result_type(operand: &FortranType) -> Option<FortranType> {
    if operand.is_logical() {
        Some(FortranType::Logical { kind: operand.kind().unwrap_or(4) })
    } else {
        None // Error: logical operator applied to non-logical type
    }
}

/// Binary logical operators: both operands must be logical, result is logical.
pub fn binary_logical_result_type(left: &FortranType, right: &FortranType) -> Option<FortranType> {
    if left.is_logical() && right.is_logical() {
        let kind = left.kind().unwrap_or(4).max(right.kind().unwrap_or(4));
        Some(FortranType::Logical { kind })
    } else {
        None
    }
}

/// Compute the result type of any binary operation.
pub fn binary_op_result_type(op: &crate::ast::expr::BinaryOp, left: &FortranType, right: &FortranType) -> Option<FortranType> {
    use crate::ast::expr::BinaryOp;
    match op {
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
            arithmetic_result_type(left, right)
        }
        BinaryOp::Pow => power_result_type(left, right),
        BinaryOp::Concat => concat_result_type(left, right),
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le |
        BinaryOp::Gt | BinaryOp::Ge => {
            if left.is_numeric() && right.is_numeric() || left.is_character() && right.is_character() {
                Some(comparison_result_type())
            } else {
                None
            }
        }
        BinaryOp::And | BinaryOp::Or | BinaryOp::Eqv | BinaryOp::Neqv => {
            binary_logical_result_type(left, right)
        }
        BinaryOp::Defined(_) => Some(FortranType::Unknown), // user-defined ops need interface resolution
    }
}

/// Compute the result type of a unary operation.
pub fn unary_op_result_type(op: &crate::ast::expr::UnaryOp, operand: &FortranType) -> Option<FortranType> {
    use crate::ast::expr::UnaryOp;
    match op {
        UnaryOp::Plus | UnaryOp::Minus => {
            if operand.is_numeric() { Some(operand.clone()) } else { None }
        }
        UnaryOp::Not => logical_result_type(operand),
        UnaryOp::Defined(_) => Some(FortranType::Unknown),
    }
}

/// What kind of entity is `A(I)` — array element, function call, or substring?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallKind {
    ArrayElement,
    FunctionCall,
    Substring,
    Unknown,
}

/// Disambiguate `A(I)` based on symbol table information.
pub fn disambiguate_call(sym_kind: &super::symtab::SymbolKind, has_range_subscript: bool) -> CallKind {
    use super::symtab::SymbolKind;
    match sym_kind {
        SymbolKind::Variable => {
            if has_range_subscript {
                CallKind::Substring // character variable with range → substring
            } else {
                CallKind::ArrayElement // variable with subscripts → array element
            }
        }
        SymbolKind::Function | SymbolKind::ExternalProc |
        SymbolKind::IntrinsicProc | SymbolKind::ProcedurePointer => CallKind::FunctionCall,
        SymbolKind::NamedInterface => CallKind::FunctionCall, // generic → function call
        _ => CallKind::Unknown,
    }
}

/// Convert a symbol table TypeInfo to a FortranType.
pub fn type_info_to_fortran_type(info: &super::symtab::TypeInfo) -> FortranType {
    use super::symtab::TypeInfo;
    match info {
        TypeInfo::Integer { kind } => FortranType::Integer { kind: kind.unwrap_or(4) },
        TypeInfo::Real { kind } => FortranType::Real { kind: kind.unwrap_or(4) },
        TypeInfo::DoublePrecision => FortranType::Real { kind: 8 },
        TypeInfo::Complex { kind } => FortranType::Complex { kind: kind.unwrap_or(4) },
        TypeInfo::Logical { kind } => FortranType::Logical { kind: kind.unwrap_or(4) },
        TypeInfo::Character { len, kind } => FortranType::Character {
            kind: kind.unwrap_or(1),
            len: match len {
                Some(n) => CharLen::Known(*n),
                None => CharLen::Unknown,
            },
        },
        TypeInfo::Derived(name) => FortranType::Derived { name: name.clone() },
        TypeInfo::Class(name) => FortranType::ClassOf { base: name.clone() },
        TypeInfo::ClassStar => FortranType::UnlimitedPoly,
        TypeInfo::TypeStar => FortranType::AssumedType,
    }
}

/// Convert an ImplicitType to a FortranType.
pub fn implicit_to_fortran_type(itype: super::symtab::ImplicitType) -> FortranType {
    use super::symtab::ImplicitType;
    match itype {
        ImplicitType::Integer => FortranType::default_integer(),
        ImplicitType::Real => FortranType::default_real(),
        ImplicitType::DoublePrecision => FortranType::double_precision(),
        ImplicitType::Complex => FortranType::default_complex(),
        ImplicitType::Logical => FortranType::default_logical(),
        ImplicitType::Character => FortranType::default_character(),
    }
}

/// Compute the type of a literal expression node.
pub fn literal_type(expr: &crate::ast::expr::Expr) -> FortranType {
    use crate::ast::expr::Expr;
    match expr {
        Expr::IntegerLiteral { kind, .. } => {
            let k = kind.as_ref().and_then(|s| s.parse::<u8>().ok()).unwrap_or(4);
            FortranType::Integer { kind: k }
        }
        Expr::RealLiteral { text, kind, .. } => {
            if let Some(k) = kind.as_ref().and_then(|s| s.parse::<u8>().ok()) {
                FortranType::Real { kind: k }
            } else {
                // d-exponent → double precision
                let lower = text.to_lowercase();
                if lower.contains('d') {
                    FortranType::Real { kind: 8 }
                } else {
                    FortranType::Real { kind: 4 }
                }
            }
        }
        Expr::StringLiteral { kind, value, .. } => {
            let k = kind.as_ref().and_then(|s| s.parse::<u8>().ok()).unwrap_or(1);
            FortranType::Character { kind: k, len: CharLen::Known(value.len() as i64) }
        }
        Expr::LogicalLiteral { kind, .. } => {
            let k = kind.as_ref().and_then(|s| s.parse::<u8>().ok()).unwrap_or(4);
            FortranType::Logical { kind: k }
        }
        Expr::ComplexLiteral { .. } => FortranType::default_complex(),
        Expr::BozLiteral { .. } => FortranType::default_integer(), // BOZ in context determines type
        _ => FortranType::Unknown,
    }
}

/// Compute the type of an expression given a symbol table context.
/// Returns Unknown for expressions that can't be resolved without more context.
pub fn expr_type(expr: &crate::ast::expr::SpannedExpr, symtab: &super::symtab::SymbolTable) -> FortranType {
    use crate::ast::expr::Expr;
    match &expr.node {
        // Literals
        Expr::IntegerLiteral { .. } | Expr::RealLiteral { .. } |
        Expr::StringLiteral { .. } | Expr::LogicalLiteral { .. } |
        Expr::ComplexLiteral { .. } | Expr::BozLiteral { .. } => literal_type(&expr.node),

        // Name — look up in symbol table
        Expr::Name { name } => {
            if let Some(sym) = symtab.lookup(name) {
                match &sym.type_info {
                    Some(info) => type_info_to_fortran_type(info),
                    None => FortranType::Unknown,
                }
            } else if let Some(itype) = symtab.implicit_type(name) {
                implicit_to_fortran_type(itype)
            } else {
                FortranType::Unknown
            }
        }

        // Component access: base%component — would need derived type definitions to resolve
        Expr::ComponentAccess { .. } => FortranType::Unknown,

        // Unary operations
        Expr::UnaryOp { op, operand } => {
            let operand_ty = expr_type(operand, symtab);
            unary_op_result_type(op, &operand_ty).unwrap_or(FortranType::Unknown)
        }

        // Binary operations
        Expr::BinaryOp { op, left, right } => {
            let left_ty = expr_type(left, symtab);
            let right_ty = expr_type(right, symtab);
            binary_op_result_type(op, &left_ty, &right_ty).unwrap_or(FortranType::Unknown)
        }

        // Function call / array access — disambiguate based on callee
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                // Check intrinsics first
                let arg_types: Vec<FortranType> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => expr_type(e, symtab),
                        crate::ast::expr::SectionSubscript::Range { .. } => FortranType::Unknown,
                    }
                }).collect();

                if let Some(result) = intrinsic_result_type(name, &arg_types) {
                    return result;
                }

                // Look up in symbol table
                if let Some(sym) = symtab.lookup(name) {
                    let has_range = args.iter().any(|a| {
                        matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. })
                    });
                    match disambiguate_call(&sym.kind, has_range) {
                        CallKind::ArrayElement => {
                            // Array element has the element type
                            match &sym.type_info {
                                Some(info) => type_info_to_fortran_type(info),
                                None => FortranType::Unknown,
                            }
                        }
                        CallKind::FunctionCall => {
                            // Return type of function
                            match &sym.type_info {
                                Some(info) => type_info_to_fortran_type(info),
                                None => FortranType::Unknown,
                            }
                        }
                        CallKind::Substring => {
                            FortranType::Character { kind: 1, len: CharLen::Unknown }
                        }
                        CallKind::Unknown => FortranType::Unknown,
                    }
                } else {
                    FortranType::Unknown
                }
            } else {
                FortranType::Unknown
            }
        }

        // Array constructor — type of first element (all elements should match)
        Expr::ArrayConstructor { values, type_spec } => {
            if type_spec.is_some() {
                // Typed array constructor — would need to resolve type_spec
                FortranType::Unknown
            } else if let Some(crate::ast::expr::AcValue::Expr(first)) = values.first() {
                expr_type(first, symtab)
            } else {
                FortranType::Unknown
            }
        }

        // Parenthesized
        Expr::ParenExpr { inner } => expr_type(inner, symtab),
    }
}

/// A dummy argument descriptor for argument matching.
#[derive(Debug, Clone)]
pub struct DummyArgDesc {
    pub name: String,
    pub type_: FortranType,
    pub intent: Option<super::symtab::Intent>,
    pub optional: bool,
}

/// A specific procedure in a generic interface.
#[derive(Debug, Clone)]
pub struct SpecificProc {
    pub name: String,
    pub dummy_args: Vec<DummyArgDesc>,
    pub result_type: FortranType,
}

/// Check actual arguments against dummy argument specifications.
/// Returns a list of errors (empty = success).
pub fn check_arguments(
    dummy_args: &[DummyArgDesc],
    actual_args: &[(Option<String>, FortranType)],
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut matched = vec![false; dummy_args.len()];

    // Phase 1: match positional arguments
    let mut pos = 0;
    for (i, (keyword, actual_type)) in actual_args.iter().enumerate() {
        if let Some(kw) = keyword {
            // Keyword argument — find matching dummy
            if let Some(idx) = dummy_args.iter().position(|d| d.name.eq_ignore_ascii_case(kw)) {
                if matched[idx] {
                    errors.push(format!("duplicate keyword argument '{}'", kw));
                } else {
                    matched[idx] = true;
                    check_arg_type(&dummy_args[idx], actual_type, &mut errors);
                }
            } else {
                errors.push(format!("unknown keyword argument '{}'", kw));
            }
        } else {
            // Positional — match to next unmatched dummy
            while pos < dummy_args.len() && matched[pos] {
                pos += 1;
            }
            if pos < dummy_args.len() {
                matched[pos] = true;
                check_arg_type(&dummy_args[pos], actual_type, &mut errors);
                pos += 1;
            } else {
                errors.push(format!("too many arguments (expected at most {})", dummy_args.len()));
                break;
            }
        }
        let _ = i; // suppress unused
    }

    // Phase 2: check that all non-optional dummies were supplied
    for (i, dummy) in dummy_args.iter().enumerate() {
        if !matched[i] && !dummy.optional {
            errors.push(format!("missing required argument '{}'", dummy.name));
        }
    }

    errors
}

/// Check a single actual argument against its dummy.
fn check_arg_type(dummy: &DummyArgDesc, actual: &FortranType, errors: &mut Vec<String>) {
    use super::symtab::Intent;

    // Skip type checking if either side is unknown
    if matches!(actual, FortranType::Unknown) || matches!(dummy.type_, FortranType::Unknown) {
        return;
    }

    // Check type compatibility
    if actual != &dummy.type_ {
        // Allow numeric conversions
        if actual.is_numeric() && dummy.type_.is_numeric() {
            // Implicit conversion allowed for intent(in)
            return;
        }
        errors.push(format!(
            "type mismatch for '{}': expected {:?}, got {:?}",
            dummy.name, dummy.type_, actual
        ));
    }

    // Check intent: can't pass a literal/expression to intent(out/inout)
    // This is a simplified check — full check needs lvalue analysis
    if matches!(dummy.intent, Some(Intent::Out) | Some(Intent::InOut)) {
        // Would need to check if actual is an lvalue — deferred to expression analysis
    }
}

/// Resolve a generic interface call to a specific procedure.
/// Returns the index of the matching specific, or an error.
pub fn resolve_generic(
    specifics: &[SpecificProc],
    actual_types: &[FortranType],
) -> Result<usize, String> {
    let mut matches = Vec::new();

    for (i, spec) in specifics.iter().enumerate() {
        if is_specific_match(&spec.dummy_args, actual_types) {
            matches.push(i);
        }
    }

    match matches.len() {
        0 => Err("no matching specific procedure for generic call".into()),
        1 => Ok(matches[0]),
        _ => Err("ambiguous generic call: multiple specifics match".into()),
    }
}

/// Check if actual argument types match a specific procedure's dummy args.
fn is_specific_match(dummy_args: &[DummyArgDesc], actual_types: &[FortranType]) -> bool {
    // Count required dummies
    let required = dummy_args.iter().filter(|d| !d.optional).count();
    if actual_types.len() < required || actual_types.len() > dummy_args.len() {
        return false;
    }

    for (actual, dummy) in actual_types.iter().zip(dummy_args.iter()) {
        if matches!(actual, FortranType::Unknown) || matches!(dummy.type_, FortranType::Unknown) {
            continue; // can't reject on unknown
        }
        if actual != &dummy.type_ {
            // Exact type match required for generic disambiguation
            // (unlike general argument checking which allows numeric conversion)
            return false;
        }
    }
    true
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

    #[test]
    fn int_power_complex() {
        // integer ** complex → complex (base promoted to real, then real+complex → complex)
        let result = power_result_type(
            &FortranType::Integer { kind: 4 },
            &FortranType::Complex { kind: 8 },
        ).unwrap();
        assert_eq!(result, FortranType::Complex { kind: 8 });
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

    // ---- Binary op result type ----

    #[test]
    fn binary_op_add_int_real() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::Add,
            &FortranType::Integer { kind: 4 },
            &FortranType::Real { kind: 8 },
        ).unwrap();
        assert_eq!(result, FortranType::Real { kind: 8 });
    }

    #[test]
    fn binary_op_pow() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::Pow,
            &FortranType::Real { kind: 4 },
            &FortranType::Integer { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Real { kind: 4 });
    }

    #[test]
    fn binary_op_concat() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::Concat,
            &FortranType::Character { kind: 1, len: CharLen::Known(3) },
            &FortranType::Character { kind: 1, len: CharLen::Known(4) },
        ).unwrap();
        assert_eq!(result, FortranType::Character { kind: 1, len: CharLen::Known(7) });
    }

    #[test]
    fn binary_op_eq_numeric() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::Eq,
            &FortranType::Integer { kind: 4 },
            &FortranType::Real { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::default_logical());
    }

    #[test]
    fn binary_op_eq_character() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::Lt,
            &FortranType::Character { kind: 1, len: CharLen::Known(5) },
            &FortranType::Character { kind: 1, len: CharLen::Known(5) },
        ).unwrap();
        assert_eq!(result, FortranType::default_logical());
    }

    #[test]
    fn binary_op_eq_mixed_types_returns_none() {
        use crate::ast::expr::BinaryOp;
        assert!(binary_op_result_type(
            &BinaryOp::Eq,
            &FortranType::Integer { kind: 4 },
            &FortranType::Character { kind: 1, len: CharLen::Known(3) },
        ).is_none());
    }

    #[test]
    fn binary_op_and() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::And,
            &FortranType::Logical { kind: 4 },
            &FortranType::Logical { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Logical { kind: 4 });
    }

    #[test]
    fn binary_op_and_non_logical_returns_none() {
        use crate::ast::expr::BinaryOp;
        assert!(binary_op_result_type(
            &BinaryOp::And,
            &FortranType::Integer { kind: 4 },
            &FortranType::Logical { kind: 4 },
        ).is_none());
    }

    #[test]
    fn binary_op_or_mixed_kind() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::Or,
            &FortranType::Logical { kind: 1 },
            &FortranType::Logical { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Logical { kind: 4 });
    }

    #[test]
    fn binary_op_defined() {
        use crate::ast::expr::BinaryOp;
        let result = binary_op_result_type(
            &BinaryOp::Defined("cross".into()),
            &FortranType::default_real(),
            &FortranType::default_real(),
        ).unwrap();
        assert_eq!(result, FortranType::Unknown);
    }

    // ---- Unary op result type ----

    #[test]
    fn unary_minus_real() {
        use crate::ast::expr::UnaryOp;
        let result = unary_op_result_type(
            &UnaryOp::Minus,
            &FortranType::Real { kind: 8 },
        ).unwrap();
        assert_eq!(result, FortranType::Real { kind: 8 });
    }

    #[test]
    fn unary_plus_non_numeric_returns_none() {
        use crate::ast::expr::UnaryOp;
        assert!(unary_op_result_type(
            &UnaryOp::Plus,
            &FortranType::default_logical(),
        ).is_none());
    }

    #[test]
    fn unary_not_logical() {
        use crate::ast::expr::UnaryOp;
        let result = unary_op_result_type(
            &UnaryOp::Not,
            &FortranType::Logical { kind: 4 },
        ).unwrap();
        assert_eq!(result, FortranType::Logical { kind: 4 });
    }

    #[test]
    fn unary_not_non_logical_returns_none() {
        use crate::ast::expr::UnaryOp;
        assert!(unary_op_result_type(
            &UnaryOp::Not,
            &FortranType::Integer { kind: 4 },
        ).is_none());
    }

    // ---- Disambiguation ----

    #[test]
    fn disambiguate_variable_element() {
        use super::super::symtab::SymbolKind;
        assert_eq!(disambiguate_call(&SymbolKind::Variable, false), CallKind::ArrayElement);
    }

    #[test]
    fn disambiguate_variable_range_is_substring() {
        use super::super::symtab::SymbolKind;
        assert_eq!(disambiguate_call(&SymbolKind::Variable, true), CallKind::Substring);
    }

    #[test]
    fn disambiguate_function() {
        use super::super::symtab::SymbolKind;
        assert_eq!(disambiguate_call(&SymbolKind::Function, false), CallKind::FunctionCall);
    }

    #[test]
    fn disambiguate_external_proc() {
        use super::super::symtab::SymbolKind;
        assert_eq!(disambiguate_call(&SymbolKind::ExternalProc, false), CallKind::FunctionCall);
    }

    #[test]
    fn disambiguate_intrinsic_proc() {
        use super::super::symtab::SymbolKind;
        assert_eq!(disambiguate_call(&SymbolKind::IntrinsicProc, true), CallKind::FunctionCall);
    }

    #[test]
    fn disambiguate_named_interface() {
        use super::super::symtab::SymbolKind;
        assert_eq!(disambiguate_call(&SymbolKind::NamedInterface, false), CallKind::FunctionCall);
    }

    #[test]
    fn disambiguate_unknown_kind() {
        use super::super::symtab::SymbolKind;
        assert_eq!(disambiguate_call(&SymbolKind::Module, false), CallKind::Unknown);
    }

    // ---- TypeInfo → FortranType conversion ----

    #[test]
    fn type_info_integer_default() {
        use super::super::symtab::TypeInfo;
        assert_eq!(
            type_info_to_fortran_type(&TypeInfo::Integer { kind: None }),
            FortranType::Integer { kind: 4 }
        );
    }

    #[test]
    fn type_info_integer_kind8() {
        use super::super::symtab::TypeInfo;
        assert_eq!(
            type_info_to_fortran_type(&TypeInfo::Integer { kind: Some(8) }),
            FortranType::Integer { kind: 8 }
        );
    }

    #[test]
    fn type_info_double_precision() {
        use super::super::symtab::TypeInfo;
        assert_eq!(
            type_info_to_fortran_type(&TypeInfo::DoublePrecision),
            FortranType::Real { kind: 8 }
        );
    }

    #[test]
    fn type_info_character_with_len() {
        use super::super::symtab::TypeInfo;
        assert_eq!(
            type_info_to_fortran_type(&TypeInfo::Character { len: Some(10), kind: None }),
            FortranType::Character { kind: 1, len: CharLen::Known(10) }
        );
    }

    #[test]
    fn type_info_derived() {
        use super::super::symtab::TypeInfo;
        assert_eq!(
            type_info_to_fortran_type(&TypeInfo::Derived("point".into())),
            FortranType::Derived { name: "point".into() }
        );
    }

    #[test]
    fn type_info_class_star() {
        use super::super::symtab::TypeInfo;
        assert_eq!(
            type_info_to_fortran_type(&TypeInfo::ClassStar),
            FortranType::UnlimitedPoly
        );
    }

    // ---- Literal type ----

    #[test]
    fn literal_type_integer() {
        use crate::ast::expr::Expr;
        assert_eq!(
            literal_type(&Expr::IntegerLiteral { text: "42".into(), kind: None }),
            FortranType::Integer { kind: 4 }
        );
    }

    #[test]
    fn literal_type_integer_kind8() {
        use crate::ast::expr::Expr;
        assert_eq!(
            literal_type(&Expr::IntegerLiteral { text: "42".into(), kind: Some("8".into()) }),
            FortranType::Integer { kind: 8 }
        );
    }

    #[test]
    fn literal_type_real_default() {
        use crate::ast::expr::Expr;
        assert_eq!(
            literal_type(&Expr::RealLiteral { text: "3.14".into(), kind: None }),
            FortranType::Real { kind: 4 }
        );
    }

    #[test]
    fn literal_type_real_d_exponent() {
        use crate::ast::expr::Expr;
        assert_eq!(
            literal_type(&Expr::RealLiteral { text: "1.0d0".into(), kind: None }),
            FortranType::Real { kind: 8 }
        );
    }

    #[test]
    fn literal_type_string() {
        use crate::ast::expr::Expr;
        assert_eq!(
            literal_type(&Expr::StringLiteral { value: "hello".into(), kind: None }),
            FortranType::Character { kind: 1, len: CharLen::Known(5) }
        );
    }

    #[test]
    fn literal_type_logical() {
        use crate::ast::expr::Expr;
        assert_eq!(
            literal_type(&Expr::LogicalLiteral { value: true, kind: None }),
            FortranType::Logical { kind: 4 }
        );
    }

    // ---- Expression type walker ----

    #[test]
    fn expr_type_integer_literal() {
        use crate::ast::expr::Expr;
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };
        let expr = Spanned::new(Expr::IntegerLiteral { text: "42".into(), kind: None }, span);
        let st = super::super::symtab::SymbolTable::new();
        assert_eq!(expr_type(&expr, &st), FortranType::Integer { kind: 4 });
    }

    #[test]
    fn expr_type_name_lookup() {
        use crate::ast::expr::Expr;
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        use super::super::symtab::*;
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };

        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.define(Symbol {
            name: "x".into(),
            kind: SymbolKind::Variable,
            type_info: Some(TypeInfo::Real { kind: Some(8) }),
            attrs: SymbolAttrs::default(),
            defined_at: span,
            scope: 0,
        }).unwrap();

        let expr = Spanned::new(Expr::Name { name: "x".into() }, span);
        assert_eq!(expr_type(&expr, &st), FortranType::Real { kind: 8 });
    }

    #[test]
    fn expr_type_name_implicit() {
        use crate::ast::expr::Expr;
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        use super::super::symtab::*;
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };

        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        // "n" is in I-N range → implicit integer
        let expr = Spanned::new(Expr::Name { name: "n".into() }, span);
        assert_eq!(expr_type(&expr, &st), FortranType::default_integer());
    }

    #[test]
    fn expr_type_binary_add() {
        use crate::ast::expr::{Expr, BinaryOp};
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };
        let st = super::super::symtab::SymbolTable::new();

        let left = Box::new(Spanned::new(Expr::IntegerLiteral { text: "1".into(), kind: None }, span));
        let right = Box::new(Spanned::new(Expr::RealLiteral { text: "2.0".into(), kind: None }, span));
        let expr = Spanned::new(Expr::BinaryOp { op: BinaryOp::Add, left, right }, span);
        assert_eq!(expr_type(&expr, &st), FortranType::Real { kind: 4 });
    }

    #[test]
    fn expr_type_unary_minus() {
        use crate::ast::expr::{Expr, UnaryOp};
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };
        let st = super::super::symtab::SymbolTable::new();

        let operand = Box::new(Spanned::new(Expr::RealLiteral { text: "3.14".into(), kind: None }, span));
        let expr = Spanned::new(Expr::UnaryOp { op: UnaryOp::Minus, operand }, span);
        assert_eq!(expr_type(&expr, &st), FortranType::Real { kind: 4 });
    }

    #[test]
    fn expr_type_intrinsic_call() {
        use crate::ast::expr::{Expr, Argument, SectionSubscript};
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };
        let st = super::super::symtab::SymbolTable::new();

        let callee = Box::new(Spanned::new(Expr::Name { name: "abs".into() }, span));
        let arg = Argument {
            keyword: None,
            value: SectionSubscript::Element(Spanned::new(
                Expr::IntegerLiteral { text: "-5".into(), kind: None }, span
            )),
        };
        let expr = Spanned::new(Expr::FunctionCall { callee, args: vec![arg] }, span);
        assert_eq!(expr_type(&expr, &st), FortranType::Integer { kind: 4 });
    }

    #[test]
    fn expr_type_paren() {
        use crate::ast::expr::Expr;
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };
        let st = super::super::symtab::SymbolTable::new();

        let inner = Box::new(Spanned::new(Expr::RealLiteral { text: "1.0".into(), kind: None }, span));
        let expr = Spanned::new(Expr::ParenExpr { inner }, span);
        assert_eq!(expr_type(&expr, &st), FortranType::Real { kind: 4 });
    }

    #[test]
    fn expr_type_array_element() {
        use crate::ast::expr::{Expr, Argument, SectionSubscript};
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        use super::super::symtab::*;
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };

        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.define(Symbol {
            name: "arr".into(),
            kind: SymbolKind::Variable,
            type_info: Some(TypeInfo::Real { kind: Some(8) }),
            attrs: SymbolAttrs::default(),
            defined_at: span,
            scope: 0,
        }).unwrap();

        let callee = Box::new(Spanned::new(Expr::Name { name: "arr".into() }, span));
        let arg = Argument {
            keyword: None,
            value: SectionSubscript::Element(Spanned::new(
                Expr::IntegerLiteral { text: "3".into(), kind: None }, span
            )),
        };
        let expr = Spanned::new(Expr::FunctionCall { callee, args: vec![arg] }, span);
        // arr(3) where arr is a variable → array element → real(8)
        assert_eq!(expr_type(&expr, &st), FortranType::Real { kind: 8 });
    }

    #[test]
    fn expr_type_substring() {
        use crate::ast::expr::{Expr, Argument, SectionSubscript};
        use crate::ast::Spanned;
        use crate::lexer::{Span, Position};
        use super::super::symtab::*;
        let span = Span { file_id: 0, start: Position { line: 1, col: 1 }, end: Position { line: 1, col: 1 } };

        let mut st = SymbolTable::new();
        st.push_scope(ScopeKind::Program("main".into()));
        st.define(Symbol {
            name: "s".into(),
            kind: SymbolKind::Variable,
            type_info: Some(TypeInfo::Character { len: Some(20), kind: None }),
            attrs: SymbolAttrs::default(),
            defined_at: span,
            scope: 0,
        }).unwrap();

        let callee = Box::new(Spanned::new(Expr::Name { name: "s".into() }, span));
        let arg = Argument {
            keyword: None,
            value: SectionSubscript::Range {
                start: Some(Spanned::new(Expr::IntegerLiteral { text: "1".into(), kind: None }, span)),
                end: Some(Spanned::new(Expr::IntegerLiteral { text: "5".into(), kind: None }, span)),
                stride: None,
            },
        };
        let expr = Spanned::new(Expr::FunctionCall { callee, args: vec![arg] }, span);
        // s(1:5) where s is character variable → substring
        assert!(expr_type(&expr, &st).is_character());
    }

    // ---- Argument matching ----

    #[test]
    fn check_args_positional_ok() {
        use super::super::symtab::Intent;
        let dummies = vec![
            DummyArgDesc { name: "a".into(), type_: FortranType::Real { kind: 4 }, intent: Some(Intent::In), optional: false },
            DummyArgDesc { name: "n".into(), type_: FortranType::Integer { kind: 4 }, intent: Some(Intent::In), optional: false },
        ];
        let actuals = vec![
            (None, FortranType::Real { kind: 4 }),
            (None, FortranType::Integer { kind: 4 }),
        ];
        assert!(check_arguments(&dummies, &actuals).is_empty());
    }

    #[test]
    fn check_args_keyword_ok() {
        use super::super::symtab::Intent;
        let dummies = vec![
            DummyArgDesc { name: "a".into(), type_: FortranType::Real { kind: 4 }, intent: Some(Intent::In), optional: false },
            DummyArgDesc { name: "n".into(), type_: FortranType::Integer { kind: 4 }, intent: Some(Intent::In), optional: false },
        ];
        let actuals = vec![
            (Some("n".into()), FortranType::Integer { kind: 4 }),
            (Some("a".into()), FortranType::Real { kind: 4 }),
        ];
        assert!(check_arguments(&dummies, &actuals).is_empty());
    }

    #[test]
    fn check_args_optional_omitted_ok() {
        let dummies = vec![
            DummyArgDesc { name: "x".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
            DummyArgDesc { name: "verbose".into(), type_: FortranType::default_logical(), intent: None, optional: true },
        ];
        let actuals = vec![
            (None, FortranType::Real { kind: 4 }),
        ];
        assert!(check_arguments(&dummies, &actuals).is_empty());
    }

    #[test]
    fn check_args_missing_required() {
        let dummies = vec![
            DummyArgDesc { name: "x".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
            DummyArgDesc { name: "y".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
        ];
        let actuals = vec![
            (None, FortranType::Real { kind: 4 }),
        ];
        let errs = check_arguments(&dummies, &actuals);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("missing required argument 'y'"));
    }

    #[test]
    fn check_args_too_many() {
        let dummies = vec![
            DummyArgDesc { name: "x".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
        ];
        let actuals = vec![
            (None, FortranType::Real { kind: 4 }),
            (None, FortranType::Real { kind: 4 }),
        ];
        let errs = check_arguments(&dummies, &actuals);
        assert!(!errs.is_empty());
        assert!(errs[0].contains("too many arguments"));
    }

    #[test]
    fn check_args_type_mismatch() {
        let dummies = vec![
            DummyArgDesc { name: "x".into(), type_: FortranType::default_logical(), intent: None, optional: false },
        ];
        let actuals = vec![
            (None, FortranType::Integer { kind: 4 }),
        ];
        let errs = check_arguments(&dummies, &actuals);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("type mismatch"));
    }

    #[test]
    fn check_args_numeric_conversion_allowed() {
        let dummies = vec![
            DummyArgDesc { name: "x".into(), type_: FortranType::Real { kind: 8 }, intent: None, optional: false },
        ];
        let actuals = vec![
            (None, FortranType::Integer { kind: 4 }),
        ];
        // integer → real conversion allowed
        assert!(check_arguments(&dummies, &actuals).is_empty());
    }

    #[test]
    fn check_args_duplicate_keyword() {
        let dummies = vec![
            DummyArgDesc { name: "x".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
        ];
        let actuals = vec![
            (Some("x".into()), FortranType::Real { kind: 4 }),
            (Some("x".into()), FortranType::Real { kind: 4 }),
        ];
        let errs = check_arguments(&dummies, &actuals);
        assert!(errs.iter().any(|e| e.contains("duplicate keyword")));
    }

    #[test]
    fn check_args_unknown_keyword() {
        let dummies = vec![
            DummyArgDesc { name: "x".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
        ];
        let actuals = vec![
            (Some("bogus".into()), FortranType::Real { kind: 4 }),
        ];
        let errs = check_arguments(&dummies, &actuals);
        assert!(errs.iter().any(|e| e.contains("unknown keyword")));
    }

    // ---- Generic resolution ----

    #[test]
    fn resolve_generic_simple() {
        let specifics = vec![
            SpecificProc {
                name: "swap_int".into(),
                dummy_args: vec![
                    DummyArgDesc { name: "a".into(), type_: FortranType::Integer { kind: 4 }, intent: None, optional: false },
                    DummyArgDesc { name: "b".into(), type_: FortranType::Integer { kind: 4 }, intent: None, optional: false },
                ],
                result_type: FortranType::Void,
            },
            SpecificProc {
                name: "swap_real".into(),
                dummy_args: vec![
                    DummyArgDesc { name: "a".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
                    DummyArgDesc { name: "b".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
                ],
                result_type: FortranType::Void,
            },
        ];

        // integer args → swap_int (index 0)
        assert_eq!(resolve_generic(&specifics, &[
            FortranType::Integer { kind: 4 }, FortranType::Integer { kind: 4 }
        ]).unwrap(), 0);

        // real args → swap_real (index 1)
        assert_eq!(resolve_generic(&specifics, &[
            FortranType::Real { kind: 4 }, FortranType::Real { kind: 4 }
        ]).unwrap(), 1);
    }

    #[test]
    fn resolve_generic_no_match() {
        let specifics = vec![
            SpecificProc {
                name: "foo_int".into(),
                dummy_args: vec![
                    DummyArgDesc { name: "x".into(), type_: FortranType::Integer { kind: 4 }, intent: None, optional: false },
                ],
                result_type: FortranType::Void,
            },
        ];
        assert!(resolve_generic(&specifics, &[FortranType::Real { kind: 4 }]).is_err());
    }

    #[test]
    fn resolve_generic_mixed_args_no_match() {
        let specifics = vec![
            SpecificProc {
                name: "swap_int".into(),
                dummy_args: vec![
                    DummyArgDesc { name: "a".into(), type_: FortranType::Integer { kind: 4 }, intent: None, optional: false },
                    DummyArgDesc { name: "b".into(), type_: FortranType::Integer { kind: 4 }, intent: None, optional: false },
                ],
                result_type: FortranType::Void,
            },
            SpecificProc {
                name: "swap_real".into(),
                dummy_args: vec![
                    DummyArgDesc { name: "a".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
                    DummyArgDesc { name: "b".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
                ],
                result_type: FortranType::Void,
            },
        ];
        // mixed integer + real → no match (exact type required for disambiguation)
        assert!(resolve_generic(&specifics, &[
            FortranType::Integer { kind: 4 }, FortranType::Real { kind: 4 }
        ]).is_err());
    }

    #[test]
    fn resolve_generic_with_optional() {
        let specifics = vec![
            SpecificProc {
                name: "process".into(),
                dummy_args: vec![
                    DummyArgDesc { name: "x".into(), type_: FortranType::Real { kind: 4 }, intent: None, optional: false },
                    DummyArgDesc { name: "mask".into(), type_: FortranType::default_logical(), intent: None, optional: true },
                ],
                result_type: FortranType::Void,
            },
        ];
        // Only required arg supplied — should match
        assert_eq!(resolve_generic(&specifics, &[
            FortranType::Real { kind: 4 }
        ]).unwrap(), 0);
    }

    // ---- Logical result type ----

    #[test]
    fn logical_result_type_ok() {
        let result = logical_result_type(&FortranType::Logical { kind: 4 }).unwrap();
        assert_eq!(result, FortranType::Logical { kind: 4 });
    }

    #[test]
    fn logical_result_type_non_logical_fails() {
        assert!(logical_result_type(&FortranType::Integer { kind: 4 }).is_none());
    }

    #[test]
    fn binary_logical_kind_promotion() {
        let result = binary_logical_result_type(
            &FortranType::Logical { kind: 1 },
            &FortranType::Logical { kind: 8 },
        ).unwrap();
        assert_eq!(result, FortranType::Logical { kind: 8 });
    }
}
