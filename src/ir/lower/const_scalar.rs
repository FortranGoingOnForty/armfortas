//! Compile-time constant folding of scalar initializers.
//!
//! `ConstScalar` is the small `Int(i128) | Float(f64)` value the
//! initializer-folder produces; `eval_const_scalar` walks the AST
//! and folds where possible (literals, paren, unary, the basic
//! binary ops, and a curated set of pure intrinsics required to fold
//! parameter initializers in the standard library). The two
//! materialization helpers — `materialize_const_scalar` and
//! `clamp_const_to_type` — bridge folded values back into IR.
//!
//! Extracted from `lower::core` in sprint 04 step 3. No behavior
//! change.

use std::collections::HashMap;

use crate::ast::expr::Expr;
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::ValueId;
use crate::ir::types::{FloatWidth, IntWidth, IrType};

/// Internal const-folding result for initializer expressions.
/// Int is used for integer kinds AND logical (0/1). Float is
/// used for real/double precision.
#[derive(Debug, Clone, Copy)]
pub(super) enum ConstScalar {
    Int(i128),
    Float(f64),
}

impl ConstScalar {
    pub(super) fn to_float(self) -> f64 {
        match self {
            ConstScalar::Int(i) => i as f64,
            ConstScalar::Float(f) => f,
        }
    }
}

pub(super) fn parse_boz_const_scalar(
    text: &str,
    base: crate::ast::expr::BozBase,
) -> Option<ConstScalar> {
    let radix = match base {
        crate::ast::expr::BozBase::Binary => 2,
        crate::ast::expr::BozBase::Octal => 8,
        crate::ast::expr::BozBase::Hex => 16,
    };
    let digits: String = text
        .chars()
        .skip_while(|c| !matches!(c, '\'' | '"'))
        .skip(1)
        .take_while(|c| !matches!(c, '\'' | '"'))
        .collect();
    i128::from_str_radix(&digits, radix)
        .ok()
        .map(ConstScalar::Int)
}

pub(super) fn eval_const_scalar(
    e: &crate::ast::expr::SpannedExpr,
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<ConstScalar> {
    use crate::ast::expr::{BinaryOp, UnaryOp};
    match &e.node {
        Expr::IntegerLiteral { text, .. } => text
            .split('_')
            .next()
            .unwrap_or(text)
            .parse::<i128>()
            .ok()
            .map(ConstScalar::Int),
        Expr::RealLiteral { text, .. } => text
            .replace('d', "e")
            .replace('D', "E")
            .split('_')
            .next()
            .unwrap_or(text)
            .parse::<f64>()
            .ok()
            .map(ConstScalar::Float),
        Expr::LogicalLiteral { value, .. } => Some(ConstScalar::Int(if *value { 1 } else { 0 })),
        Expr::BozLiteral { text, base } => parse_boz_const_scalar(text, *base),
        // Audit CRITICAL-1: a name reference resolves only if it's
        // a compile-time parameter declared earlier in the same
        // scope. Anything else (regular local, dummy arg, module
        // global) is not a compile-time constant and the folder
        // gives up — the caller falls back to runtime evaluation.
        Expr::Name { name } => param_consts.get(&name.to_lowercase()).copied(),
        Expr::UnaryOp { op, operand } => {
            let v = eval_const_scalar(operand, param_consts)?;
            match op {
                UnaryOp::Minus => Some(match v {
                    ConstScalar::Int(i) => ConstScalar::Int(-i),
                    ConstScalar::Float(f) => ConstScalar::Float(-f),
                }),
                UnaryOp::Plus => Some(v),
                _ => None,
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let lv = eval_const_scalar(left, param_consts)?;
            let rv = eval_const_scalar(right, param_consts)?;
            // Promote to float when either operand is float.
            let any_float =
                matches!(lv, ConstScalar::Float(_)) || matches!(rv, ConstScalar::Float(_));
            if any_float {
                let l = lv.to_float();
                let r = rv.to_float();
                match op {
                    BinaryOp::Add => Some(ConstScalar::Float(l + r)),
                    BinaryOp::Sub => Some(ConstScalar::Float(l - r)),
                    BinaryOp::Mul => Some(ConstScalar::Float(l * r)),
                    // Audit Min-5: fold all IEEE 754 cases. Float
                    // division by zero now folds to ±Inf or NaN
                    // (matching `f64::powf`, which already folds
                    // negative-base fractional powers to NaN).
                    // Consistent with gfortran's `parameter ::
                    // x = 1.0/0.0 → +inf` behavior.
                    BinaryOp::Div => Some(ConstScalar::Float(l / r)),
                    BinaryOp::Pow => Some(ConstScalar::Float(l.powf(r))),
                    BinaryOp::Eq => Some(ConstScalar::Int((l == r) as i128)),
                    BinaryOp::Ne => Some(ConstScalar::Int((l != r) as i128)),
                    BinaryOp::Lt => Some(ConstScalar::Int((l < r) as i128)),
                    BinaryOp::Le => Some(ConstScalar::Int((l <= r) as i128)),
                    BinaryOp::Gt => Some(ConstScalar::Int((l > r) as i128)),
                    BinaryOp::Ge => Some(ConstScalar::Int((l >= r) as i128)),
                    _ => None,
                }
            } else {
                let (ConstScalar::Int(l), ConstScalar::Int(r)) = (lv, rv) else {
                    return None;
                };
                match op {
                    BinaryOp::Add => Some(ConstScalar::Int(l.wrapping_add(r))),
                    BinaryOp::Sub => Some(ConstScalar::Int(l.wrapping_sub(r))),
                    BinaryOp::Mul => Some(ConstScalar::Int(l.wrapping_mul(r))),
                    BinaryOp::Div => {
                        if r == 0 {
                            None
                        } else {
                            Some(ConstScalar::Int(l / r))
                        }
                    }
                    BinaryOp::Pow => {
                        // Integer power with non-negative exponent.
                        if r < 0 || r > i32::MAX as i128 {
                            return None;
                        }
                        let mut acc: i128 = 1;
                        for _ in 0..r {
                            acc = acc.wrapping_mul(l);
                        }
                        Some(ConstScalar::Int(acc))
                    }
                    BinaryOp::Eq => Some(ConstScalar::Int((l == r) as i128)),
                    BinaryOp::Ne => Some(ConstScalar::Int((l != r) as i128)),
                    BinaryOp::Lt => Some(ConstScalar::Int((l < r) as i128)),
                    BinaryOp::Le => Some(ConstScalar::Int((l <= r) as i128)),
                    BinaryOp::Gt => Some(ConstScalar::Int((l > r) as i128)),
                    BinaryOp::Ge => Some(ConstScalar::Int((l >= r) as i128)),
                    BinaryOp::And => Some(ConstScalar::Int(((l != 0) && (r != 0)) as i128)),
                    BinaryOp::Or => Some(ConstScalar::Int(((l != 0) || (r != 0)) as i128)),
                    BinaryOp::Eqv => Some(ConstScalar::Int(((l != 0) == (r != 0)) as i128)),
                    BinaryOp::Neqv => Some(ConstScalar::Int(((l != 0) != (r != 0)) as i128)),
                    _ => None,
                }
            }
        }
        Expr::ParenExpr { inner } => eval_const_scalar(inner, param_consts),
        Expr::FunctionCall { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                let first_arg = args.first().and_then(|a| {
                    if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                        eval_const_scalar(e, param_consts)
                    } else {
                        None
                    }
                });
                match key.as_str() {
                    "not" => match first_arg? {
                        ConstScalar::Int(i) => Some(ConstScalar::Int(!i)),
                        ConstScalar::Float(_) => None,
                    },
                    "selected_int_kind" => {
                        if let Some(ConstScalar::Int(r)) = first_arg {
                            let r = r as i64;
                            let kind = if r <= 2 {
                                1
                            } else if r <= 4 {
                                2
                            } else if r <= 9 {
                                4
                            } else if r <= 18 {
                                8
                            } else if r <= 38 {
                                16
                            } else {
                                -1
                            };
                            Some(ConstScalar::Int(kind as i128))
                        } else {
                            None
                        }
                    }
                    "bit_size" => {
                        // F2018 §16.9.31: BIT_SIZE(I) returns the number of
                        // bits in the model integer for argument I. Folds at
                        // compile time when the argument has a known kind.
                        // Without this, module-level
                        // `integer, parameter :: K = bit_size(1_8)`
                        // initializers stored zero in .data — the linked
                        // binary then read K=0 inside any function that
                        // imported the module, breaking F2018 §13.7
                        // semantics. stdlib_random's MAX_INT_BIT_SIZE was
                        // the failing repro.
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        let kind = match &e.node {
                            Expr::IntegerLiteral { kind, .. } => match kind.as_deref() {
                                None => 4,
                                Some(s) => match s.parse::<i64>().ok() {
                                    Some(k) => k,
                                    None => {
                                        let key = s.to_lowercase();
                                        match key.as_str() {
                                            "int8" => 1,
                                            "int16" => 2,
                                            "int32" => 4,
                                            "int64" => 8,
                                            _ => match param_consts.get(&key).copied() {
                                                Some(ConstScalar::Int(v)) => v as i64,
                                                _ => return None,
                                            },
                                        }
                                    }
                                },
                            },
                            Expr::Name { name } => {
                                let key = name.to_lowercase();
                                match key.as_str() {
                                    "int8" => 1,
                                    "int16" => 2,
                                    "int32" => 4,
                                    "int64" => 8,
                                    _ => match param_consts.get(&key).copied() {
                                        Some(ConstScalar::Int(v)) => v as i64,
                                        _ => return None,
                                    },
                                }
                            }
                            _ => 4,
                        };
                        Some(ConstScalar::Int((kind as i128) * 8))
                    }
                    "selected_real_kind" => {
                        if let Some(ConstScalar::Int(p)) = first_arg {
                            let p = p as i64;
                            let kind = if p <= 6 {
                                4
                            } else if p <= 15 {
                                8
                            } else {
                                -1
                            };
                            Some(ConstScalar::Int(kind as i128))
                        } else {
                            None
                        }
                    }
                    "kind" => {
                        // kind(expr): return the kind of the argument.
                        // For compile-time purposes, infer from the literal type.
                        if let Some(arg_expr) = args.first() {
                            if let crate::ast::expr::SectionSubscript::Element(e) = &arg_expr.value
                            {
                                match &e.node {
                                    Expr::RealLiteral { text, .. } => {
                                        if text.contains('d') || text.contains('D') {
                                            Some(ConstScalar::Int(8))
                                        } else {
                                            Some(ConstScalar::Int(4))
                                        }
                                    }
                                    Expr::IntegerLiteral { .. } => Some(ConstScalar::Int(4)),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    "int" => {
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        match eval_const_scalar(e, param_consts)? {
                            ConstScalar::Int(i) => Some(ConstScalar::Int(i)),
                            ConstScalar::Float(f) => Some(ConstScalar::Int(f.trunc() as i128)),
                        }
                    }
                    "transfer" => eval_const_transfer(args, param_consts),
                    "ichar" | "iachar" => {
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        if let Expr::StringLiteral { value, .. } = &e.node {
                            let ch = value.as_bytes().first().copied().unwrap_or(0);
                            Some(ConstScalar::Int(ch as i128))
                        } else {
                            None
                        }
                    }
                    // Pure real-valued math intrinsics. F2018 §16.9 allows
                    // these in initialization expressions for PARAMETERs;
                    // without folding here the const ends up at runtime-zero
                    // because there is no module-level evaluator emitting
                    // the value into storage for non-trivial initializers.
                    "sqrt" | "dsqrt" | "exp" | "dexp" | "log" | "dlog" | "log10" | "dlog10"
                    | "sin" | "dsin" | "cos" | "dcos" | "tan" | "dtan" | "asin" | "dasin"
                    | "acos" | "dacos" | "atan" | "datan" | "sinh" | "dsinh" | "cosh" | "dcosh"
                    | "tanh" | "dtanh" => {
                        let v = first_arg?.to_float();
                        let r = match key.as_str() {
                            "sqrt" | "dsqrt" => v.sqrt(),
                            "exp" | "dexp" => v.exp(),
                            "log" | "dlog" => v.ln(),
                            "log10" | "dlog10" => v.log10(),
                            "sin" | "dsin" => v.sin(),
                            "cos" | "dcos" => v.cos(),
                            "tan" | "dtan" => v.tan(),
                            "asin" | "dasin" => v.asin(),
                            "acos" | "dacos" => v.acos(),
                            "atan" | "datan" => v.atan(),
                            "sinh" | "dsinh" => v.sinh(),
                            "cosh" | "dcosh" => v.cosh(),
                            "tanh" | "dtanh" => v.tanh(),
                            _ => return None,
                        };
                        Some(ConstScalar::Float(r))
                    }
                    "atan2" | "datan2" => {
                        let y = first_arg?.to_float();
                        let x = args.get(1).and_then(|a| {
                            if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                                eval_const_scalar(e, param_consts).map(|c| c.to_float())
                            } else {
                                None
                            }
                        })?;
                        Some(ConstScalar::Float(y.atan2(x)))
                    }
                    "abs" | "dabs" => match first_arg? {
                        ConstScalar::Float(f) => Some(ConstScalar::Float(f.abs())),
                        ConstScalar::Int(i) => Some(ConstScalar::Int(i.abs())),
                    },
                    "real" | "dble" | "dfloat" | "float" => {
                        // Drop the optional kind arg; just normalize numeric
                        // operand to a float for further folding.
                        Some(ConstScalar::Float(first_arg?.to_float()))
                    }
                    "epsilon" | "tiny" | "huge" => {
                        // F2018 §16.9.81 / §16.9.187 / §16.9.92: numeric
                        // inquiry intrinsics. Folded at compile time so
                        // module-level `parameter :: tol = epsilon(1.0_dp)`
                        // initializers store the right value rather than
                        // zero. Without this fold every dependent runtime
                        // check (Lentz convergence in stdlib_specialfunctions
                        // _gamma's gpx_*, etc.) sees tol == 0 and never
                        // exits.
                        let arg = args.first()?;
                        let crate::ast::expr::SectionSubscript::Element(e) = &arg.value else {
                            return None;
                        };
                        // Determine the operand's kind. Float literal: check
                        // suffix (1.0d0 → 8). Integer literal → 4. Named
                        // constant — look up in param_consts to recover
                        // its kind by value range.
                        enum Kind {
                            F32,
                            F64,
                            I32,
                            I64,
                        }
                        let kind = match &e.node {
                            Expr::RealLiteral { text, kind, .. } => {
                                let lower = text.to_ascii_lowercase();
                                if let Some(k) = kind.as_deref() {
                                    match k.parse::<i64>().ok() {
                                        Some(8) => Kind::F64,
                                        Some(4) => Kind::F32,
                                        _ => match k.to_ascii_lowercase().as_str() {
                                            "dp" | "real64" => Kind::F64,
                                            _ => Kind::F32,
                                        },
                                    }
                                } else if lower.contains('d') {
                                    Kind::F64
                                } else {
                                    Kind::F32
                                }
                            }
                            Expr::IntegerLiteral { kind, .. } => {
                                match kind.as_deref().and_then(|s| s.parse::<i64>().ok()) {
                                    Some(8) => Kind::I64,
                                    _ => Kind::I32,
                                }
                            }
                            _ => return None,
                        };
                        match (key.as_str(), kind) {
                            ("epsilon", Kind::F32) => Some(ConstScalar::Float(f32::EPSILON as f64)),
                            ("epsilon", Kind::F64) => Some(ConstScalar::Float(f64::EPSILON)),
                            ("tiny", Kind::F32) => {
                                Some(ConstScalar::Float(f32::MIN_POSITIVE as f64))
                            }
                            ("tiny", Kind::F64) => Some(ConstScalar::Float(f64::MIN_POSITIVE)),
                            ("huge", Kind::F32) => Some(ConstScalar::Float(f32::MAX as f64)),
                            ("huge", Kind::F64) => Some(ConstScalar::Float(f64::MAX)),
                            ("huge", Kind::I32) => Some(ConstScalar::Int(i32::MAX as i128)),
                            ("huge", Kind::I64) => Some(ConstScalar::Int(i64::MAX as i128)),
                            _ => None,
                        }
                    }
                    "max" | "min" => {
                        // Variadic — fold as long as every arg folds.
                        let mut acc: Option<ConstScalar> = None;
                        for a in args {
                            let crate::ast::expr::SectionSubscript::Element(e) = &a.value else {
                                return None;
                            };
                            let v = eval_const_scalar(e, param_consts)?;
                            acc = Some(match (acc, v) {
                                (None, v) => v,
                                (Some(ConstScalar::Float(af)), v) => {
                                    let bf = v.to_float();
                                    let r = if key == "max" { af.max(bf) } else { af.min(bf) };
                                    ConstScalar::Float(r)
                                }
                                (Some(ConstScalar::Int(ai)), ConstScalar::Int(bi)) => {
                                    let r = if key == "max" { ai.max(bi) } else { ai.min(bi) };
                                    ConstScalar::Int(r)
                                }
                                (Some(ConstScalar::Int(ai)), ConstScalar::Float(bf)) => {
                                    let af = ai as f64;
                                    let r = if key == "max" { af.max(bf) } else { af.min(bf) };
                                    ConstScalar::Float(r)
                                }
                            });
                        }
                        acc
                    }
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn const_call_arg_expr(arg: &crate::ast::expr::Argument) -> Option<&crate::ast::expr::SpannedExpr> {
    match &arg.value {
        crate::ast::expr::SectionSubscript::Element(expr) => Some(expr),
        _ => None,
    }
}

fn eval_const_transfer(
    args: &[crate::ast::expr::Argument],
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<ConstScalar> {
    if args.len() < 2 || args.iter().take(2).any(|arg| arg.keyword.is_some()) {
        return None;
    }
    if args.get(2).is_some() {
        // SIZE requests an array result; this scalar folder must not
        // collapse that to a single mold element.
        return None;
    }

    let source = const_call_arg_expr(args.first()?)?;
    let mold = const_call_arg_expr(args.get(1)?)?;
    let target_bytes = const_integer_storage_bytes(mold, param_consts)?;
    let source_bytes = const_transfer_source_bytes(source, param_consts)?;
    Some(ConstScalar::Int(read_signed_le_int(
        &source_bytes,
        target_bytes,
    )))
}

fn const_transfer_source_bytes(
    expr: &crate::ast::expr::SpannedExpr,
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<Vec<u8>> {
    match &expr.node {
        Expr::ArrayConstructor { values, .. } => {
            let mut out = Vec::new();
            for value in values {
                let crate::ast::expr::AcValue::Expr(elem) = value else {
                    return None;
                };
                let width = const_integer_storage_bytes(elem, param_consts)?;
                let scalar = eval_const_scalar(elem, param_consts)?;
                let ConstScalar::Int(raw) = scalar else {
                    return None;
                };
                append_le_int_bytes(&mut out, raw, width);
            }
            Some(out)
        }
        _ => {
            let width = const_integer_storage_bytes(expr, param_consts)?;
            let scalar = eval_const_scalar(expr, param_consts)?;
            let ConstScalar::Int(raw) = scalar else {
                return None;
            };
            let mut out = Vec::new();
            append_le_int_bytes(&mut out, raw, width);
            Some(out)
        }
    }
}

fn const_integer_storage_bytes(
    expr: &crate::ast::expr::SpannedExpr,
    param_consts: &HashMap<String, ConstScalar>,
) -> Option<usize> {
    match &expr.node {
        Expr::IntegerLiteral { kind, .. } | Expr::LogicalLiteral { kind, .. } => Some(
            kind.as_deref()
                .and_then(|k| kind_bytes(k, param_consts))
                .unwrap_or(4),
        ),
        Expr::ParenExpr { inner } => const_integer_storage_bytes(inner, param_consts),
        _ => None,
    }
}

fn kind_bytes(kind: &str, param_consts: &HashMap<String, ConstScalar>) -> Option<usize> {
    let key = kind.to_ascii_lowercase();
    if let Ok(value) = key.parse::<usize>() {
        return Some(value).filter(|b| matches!(b, 1 | 2 | 4 | 8 | 16));
    }
    match key.as_str() {
        "int8" | "c_int8_t" => Some(1),
        "int16" | "c_int16_t" => Some(2),
        "int32" | "c_int32_t" | "c_int" => Some(4),
        "int64" | "c_int64_t" | "c_long" | "c_long_long" => Some(8),
        _ => match param_consts.get(&key).copied()? {
            ConstScalar::Int(value) => usize::try_from(value)
                .ok()
                .filter(|b| matches!(b, 1 | 2 | 4 | 8 | 16)),
            ConstScalar::Float(_) => None,
        },
    }
}

fn append_le_int_bytes(out: &mut Vec<u8>, value: i128, bytes: usize) {
    for offset in 0..bytes {
        out.push(((value >> (offset * 8)) & 0xff) as u8);
    }
}

fn read_signed_le_int(bytes: &[u8], target_bytes: usize) -> i128 {
    let mut value = 0_i128;
    for idx in 0..target_bytes {
        let byte = bytes.get(idx).copied().unwrap_or(0) as i128;
        value |= byte << (idx * 8);
    }
    let bits = target_bytes * 8;
    let sign_bit = 1_i128 << (bits - 1);
    if value & sign_bit != 0 {
        value - (1_i128 << bits)
    } else {
        value
    }
}

pub(super) fn const_scalar_ir_type(value: ConstScalar) -> IrType {
    match value {
        ConstScalar::Int(v) => {
            if i32::try_from(v).is_ok() {
                IrType::Int(IntWidth::I32)
            } else {
                IrType::Int(IntWidth::I64)
            }
        }
        ConstScalar::Float(_) => IrType::Float(FloatWidth::F64),
    }
}

/// Emit IR instructions that materialize a folded constant
/// scalar at the given target type. Used by Maj4 parameter
/// inlining: when an `Expr::Name` references a parameter whose
/// initializer const-folds, we emit `b.const_i32(value)` (or
/// the appropriate width) directly instead of going through a
/// global address + load.
pub(super) fn materialize_const_scalar(
    b: &mut FuncBuilder,
    c: ConstScalar,
    target: &IrType,
) -> ValueId {
    // Complex target — materialize a 2-lane buffer with the const as
    // the real part and 0 as the imaginary part. Return the typed
    // buffer address, matching ordinary complex local lowering; the
    // backend scalar load/store path cannot safely carry a 16-byte
    // complex(real64) aggregate through a single GP register.
    //
    // Without this case `complex(sp), parameter :: alpha = 1.0_sp`
    // would fall through to `const_i32(0)`, mis-typing the parameter
    // at every reference and breaking generic dispatch on alpha
    // against complex formals.
    if let IrType::Array(elem, 2) = target {
        if let IrType::Float(fw) = elem.as_ref() {
            let fw = *fw;
            let re = match c {
                ConstScalar::Float(f) => match fw {
                    FloatWidth::F32 => b.const_f32(f as f32),
                    FloatWidth::F64 => b.const_f64(f),
                },
                ConstScalar::Int(i) => match fw {
                    FloatWidth::F32 => b.const_f32(i as f32),
                    FloatWidth::F64 => b.const_f64(i as f64),
                },
            };
            let zero = match fw {
                FloatWidth::F32 => b.const_f32(0.0),
                FloatWidth::F64 => b.const_f64(0.0),
            };
            let buf = b.alloca(IrType::Array(Box::new(IrType::Float(fw)), 2));
            let zero_off = b.const_i64(0);
            let lane_bytes = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
            let re_ptr = b.gep(buf, vec![zero_off], IrType::Int(IntWidth::I8));
            let im_ptr = b.gep(buf, vec![lane_bytes], IrType::Int(IntWidth::I8));
            b.store(re, re_ptr);
            b.store(zero, im_ptr);
            return buf;
        }
    }
    match (c, target) {
        (ConstScalar::Int(i), IrType::Int(IntWidth::I128)) => b.const_i128(i),
        (ConstScalar::Int(i), IrType::Int(IntWidth::I64)) => b.const_i64(i as i64),
        (ConstScalar::Int(i), IrType::Int(IntWidth::I32)) => b.const_i32(i as i32),
        (ConstScalar::Int(i), IrType::Int(width @ (IntWidth::I8 | IntWidth::I16))) => {
            b.const_int(i, *width)
        }
        (ConstScalar::Int(i), IrType::Bool) => b.const_bool(i != 0),
        (ConstScalar::Int(i), IrType::Float(FloatWidth::F64)) => b.const_f64(i as f64),
        (ConstScalar::Int(i), IrType::Float(FloatWidth::F32)) => b.const_f32(i as f32),
        (ConstScalar::Float(f), IrType::Float(FloatWidth::F64)) => b.const_f64(f),
        (ConstScalar::Float(f), IrType::Float(FloatWidth::F32)) => b.const_f32(f as f32),
        (ConstScalar::Float(f), IrType::Int(IntWidth::I128)) => b.const_i128(f as i128),
        (ConstScalar::Float(f), IrType::Int(IntWidth::I64)) => b.const_i64(f as i64),
        (ConstScalar::Float(f), IrType::Int(_)) => b.const_i32(f as i32),
        // Fallback — emit a zero of the target's class.
        _ => b.const_i32(0),
    }
}

/// Sign-extend an i64 const value at the target IR type's width.
/// `integer(kind=1) :: x = 256` parses to 256, which doesn't fit
/// in i8; the user almost certainly meant the truncation
/// (`256 mod 256 = 0`). Clamp by masking to the low N bits and
/// re-sign-extending. Out-of-range floats and aggregates are
/// passed through unchanged. Audit CRITICAL-2.
pub(super) fn clamp_const_to_type(v: ConstScalar, target: &IrType) -> ConstScalar {
    match (v, target) {
        (ConstScalar::Int(i), IrType::Int(IntWidth::I8)) => ConstScalar::Int((i as i8) as i128),
        (ConstScalar::Int(i), IrType::Int(IntWidth::I16)) => ConstScalar::Int((i as i16) as i128),
        (ConstScalar::Int(i), IrType::Int(IntWidth::I32)) => ConstScalar::Int((i as i32) as i128),
        (ConstScalar::Int(i), IrType::Int(IntWidth::I64)) => ConstScalar::Int((i as i64) as i128),
        (ConstScalar::Int(i), IrType::Bool) => ConstScalar::Int(if i != 0 { 1 } else { 0 }),
        // Int → Float (e.g. `real :: x = 1`).
        (ConstScalar::Int(i), IrType::Float(_)) => ConstScalar::Float(i as f64),
        _ => v,
    }
}
