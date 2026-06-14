//! Lowering of Fortran intrinsic functions to IR.
//!
//! Extracted from `core.rs` in Sprint 11 Stage B.1. Pure mechanical
//! move — behavior unchanged. Helpers consulted via `core::*`.

use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;

use super::core::*;
use super::helpers::{coerce_to_type, storage_size_bits_for_ir_type};

fn small_int_bits_as_i32(b: &mut FuncBuilder, value: ValueId, width: IntWidth) -> ValueId {
    let widened = b.int_extend(value, IntWidth::I32, true);
    let mask = match width {
        IntWidth::I8 => b.const_i32(0xff),
        IntWidth::I16 => b.const_i32(0xffff),
        _ => return widened,
    };
    b.bit_and(widened, mask)
}

fn unsigned_bit_value_for_width(b: &mut FuncBuilder, value: ValueId, width: IntWidth) -> ValueId {
    match width {
        IntWidth::I8 | IntWidth::I16 => small_int_bits_as_i32(b, value, width),
        _ => value,
    }
}

fn truncate_bit_value_to_width(b: &mut FuncBuilder, value: ValueId, width: IntWidth) -> ValueId {
    match width {
        IntWidth::I8 | IntWidth::I16 => b.int_trunc(value, width),
        _ => value,
    }
}

/// Lower a Fortran intrinsic function call to IR instructions.
/// Returns Some(ValueId) if recognized, None for external functions.
pub(crate) fn lower_intrinsic(
    b: &mut FuncBuilder,
    name: &str,
    args: &[ValueId],
) -> Option<ValueId> {
    match name {
        "cmplx" => {
            if let Some(real_arg) = args.first() {
                let kind = args
                    .get(2)
                    .and_then(|arg| extract_const_int_from_value(b, *arg))
                    .unwrap_or_else(|| {
                        if args.iter().any(|arg| {
                            let ty = b.func().value_type(*arg);
                            matches!(ty, Some(IrType::Float(FloatWidth::F64)))
                                || ty.as_ref().is_some_and(|ty| {
                                    is_complex_ty(ty) && complex_float_width(ty) == FloatWidth::F64
                                })
                        }) {
                            8
                        } else {
                            4
                        }
                    });
                let fw = if kind == 8 {
                    FloatWidth::F64
                } else {
                    FloatWidth::F32
                };
                let elem_ty = IrType::Float(fw);
                let buf = b.alloca(IrType::Array(Box::new(elem_ty.clone()), 2));
                let zero = b.const_i64(0);
                let imag_offset = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                let real_val = coerce_to_type(b, *real_arg, &elem_ty);
                let imag_val = if let Some(imag_arg) = args.get(1) {
                    coerce_to_type(b, *imag_arg, &elem_ty)
                } else {
                    match fw {
                        FloatWidth::F64 => b.const_f64(0.0),
                        FloatWidth::F32 => b.const_f32(0.0),
                    }
                };
                let real_ptr = b.gep(buf, vec![zero], IrType::Int(IntWidth::I8));
                b.store(real_val, real_ptr);
                let imag_ptr = b.gep(buf, vec![imag_offset], IrType::Int(IntWidth::I8));
                b.store(imag_val, imag_ptr);
                Some(buf)
            } else {
                None
            }
        }
        "conjg" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if is_complex_ty(&ty) {
                    let fw = complex_float_width(&ty);
                    let elem_ty = IrType::Float(fw);
                    let buf = b.alloca(IrType::Array(Box::new(elem_ty.clone()), 2));
                    let zero = b.const_i64(0);
                    let imag_offset = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                    let real_ptr = b.gep(*arg, vec![zero], IrType::Int(IntWidth::I8));
                    let imag_ptr = b.gep(*arg, vec![imag_offset], IrType::Int(IntWidth::I8));
                    let real_val = b.load_typed(real_ptr, elem_ty.clone());
                    let imag_val = b.load_typed(imag_ptr, elem_ty.clone());
                    let neg_imag = b.fneg(imag_val);
                    let out_real_ptr = b.gep(buf, vec![zero], IrType::Int(IntWidth::I8));
                    b.store(real_val, out_real_ptr);
                    let out_imag_ptr = b.gep(buf, vec![imag_offset], IrType::Int(IntWidth::I8));
                    b.store(neg_imag, out_imag_ptr);
                    Some(buf)
                } else {
                    None
                }
            } else {
                None
            }
        }
        "merge" => {
            if args.len() >= 3 {
                let t_ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let f_ty = b
                    .func()
                    .value_type(args[1])
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if is_complex_ty(&t_ty) || is_complex_ty(&f_ty) {
                    let fw = if [t_ty.clone(), f_ty.clone()]
                        .iter()
                        .any(|ty| is_complex_ty(ty) && complex_float_width(ty) == FloatWidth::F64)
                    {
                        FloatWidth::F64
                    } else {
                        FloatWidth::F32
                    };
                    let true_val = materialize_complex_operand(b, args[0], fw);
                    let false_val = materialize_complex_operand(b, args[1], fw);
                    let mask = coerce_to_type(b, args[2], &IrType::Bool);
                    return Some(b.select(mask, true_val, false_val));
                }
                let mut ty = t_ty;
                if ty.is_float() {
                    if matches!(f_ty, IrType::Float(FloatWidth::F64)) {
                        ty = IrType::Float(FloatWidth::F64);
                    }
                } else if ty.is_int() {
                    let width = [args[0], args[1]]
                        .iter()
                        .filter_map(|arg| b.func().value_type(*arg).and_then(|ty| ty.int_width()))
                        .max_by_key(|width| width.bits())
                        .unwrap_or(IntWidth::I32);
                    ty = IrType::Int(width);
                }
                let true_val = coerce_to_type(b, args[0], &ty);
                let false_val = coerce_to_type(b, args[1], &ty);
                let mask = coerce_to_type(b, args[2], &IrType::Bool);
                Some(b.select(mask, true_val, false_val))
            } else {
                None
            }
        }
        "transfer" => {
            if args.len() >= 2 {
                let mold_ty = b
                    .func()
                    .value_type(args[1])
                    .unwrap_or(IrType::Int(IntWidth::I32));
                Some(coerce_to_type(b, args[0], &mold_ty))
            } else {
                None
            }
        }
        "mod" => {
            // MOD(a, p) = a - INT(a/p) * p  (sign of dividend)
            if args.len() >= 2 {
                let (lhs, rhs) = unify_int_widths(b, args[0], args[1]);
                let ty = b
                    .func()
                    .value_type(lhs)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_float() {
                    Some(b.call(FuncRef::External("fmod".into()), vec![lhs, rhs], ty))
                } else {
                    Some(b.imod(lhs, rhs))
                }
            } else {
                None
            }
        }
        "modulo" => {
            // MODULO(a, p) = a - FLOOR(a/p) * p  (sign of divisor, result in [0, |p|))
            // For integers: if result has opposite sign to p, add p.
            if args.len() >= 2 {
                let (lhs, rhs) = unify_int_widths(b, args[0], args[1]);
                let ty = b
                    .func()
                    .value_type(lhs)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_float() {
                    // Float modulo: use fmod then adjust.
                    let rem = b.call(FuncRef::External("fmod".into()), vec![lhs, rhs], ty.clone());
                    let sum = b.fadd(rem, rhs);
                    let rem2 = b.call(FuncRef::External("fmod".into()), vec![sum, rhs], ty);
                    Some(rem2)
                } else {
                    // Integer modulo: rem = a % p; if (rem != 0 && (rem ^ p) < 0) rem += p
                    let rem = b.imod(lhs, rhs);
                    let zero = match &ty {
                        IrType::Int(IntWidth::I64) => b.const_i64(0),
                        _ => b.const_i32(0),
                    };
                    let rem_ne_zero = b.icmp(CmpOp::Ne, rem, zero);
                    let rem_xor_p = b.bit_xor(rem, rhs);
                    let sign_differs = b.icmp(CmpOp::Lt, rem_xor_p, zero);
                    let needs_adjust = b.and(rem_ne_zero, sign_differs);
                    let adjusted = b.iadd(rem, rhs);
                    Some(b.select(needs_adjust, adjusted, rem))
                }
            } else {
                None
            }
        }
        "abs" | "iabs" | "dabs" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                match &ty {
                    IrType::Int(w) => {
                        let zero = match w {
                            IntWidth::I64 => b.const_i64(0),
                            _ => b.const_i32(0),
                        };
                        let is_pos = b.icmp(CmpOp::Ge, *arg, zero);
                        let neg = b.ineg(*arg);
                        Some(b.select(is_pos, *arg, neg))
                    }
                    IrType::Float(_) => Some(b.fabs(*arg)),
                    _ => None,
                }
            } else {
                None
            }
        }
        "int" | "idint" | "ifix" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let requested_width = args
                    .get(1)
                    .and_then(|kind| extract_const_int_from_value(b, *kind))
                    .and_then(int_width_from_kind_value)
                    .unwrap_or(IntWidth::I32);
                if is_complex_ty(&ty) {
                    let fw = complex_float_width(&ty);
                    let buf = materialize_complex_operand(b, *arg, fw);
                    let zero = b.const_i64(0);
                    let re_ptr = b.gep(buf, vec![zero], IrType::Int(IntWidth::I8));
                    let re = b.load_typed(re_ptr, IrType::Float(fw));
                    Some(b.float_to_int(re, requested_width))
                } else if ty.is_float() {
                    Some(b.float_to_int(*arg, requested_width))
                } else {
                    Some(coerce_to_type(b, *arg, &IrType::Int(requested_width)))
                }
            } else {
                None
            }
        }
        "nint" | "idnint" => {
            // NINT: round to nearest integer (not truncate).
            // Round via libm round(), then convert to int.
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let requested_width = args
                    .get(1)
                    .and_then(|kind| extract_const_int_from_value(b, *kind))
                    .and_then(int_width_from_kind_value)
                    .unwrap_or(IntWidth::I32);
                if ty.is_float() {
                    let func = if matches!(ty, IrType::Float(FloatWidth::F32)) {
                        "roundf"
                    } else {
                        "round"
                    };
                    let rounded = b.call(FuncRef::External(func.into()), vec![*arg], ty.clone());
                    Some(b.float_to_int(rounded, requested_width))
                } else {
                    Some(coerce_to_type(b, *arg, &IrType::Int(requested_width)))
                }
            } else {
                None
            }
        }
        "anint" | "dnint" => {
            // ANINT: round to nearest whole number, return as real.
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let func = if matches!(ty, IrType::Float(FloatWidth::F32)) {
                    "roundf"
                } else {
                    "round"
                };
                Some(b.call(FuncRef::External(func.into()), vec![*arg], ty))
            } else {
                None
            }
        }
        "real" | "float" | "sngl" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let requested_fw = args
                    .get(1)
                    .and_then(|kind| extract_const_int_from_value(b, *kind))
                    .map(|kind| {
                        if kind == 8 {
                            FloatWidth::F64
                        } else {
                            FloatWidth::F32
                        }
                    });
                let default_fw = if matches!(ty, IrType::Float(FloatWidth::F64))
                    || (is_complex_ty(&ty) && complex_float_width(&ty) == FloatWidth::F64)
                {
                    FloatWidth::F64
                } else {
                    FloatWidth::F32
                };
                let target_ty = IrType::Float(requested_fw.unwrap_or(default_fw));
                if ty.is_int() || ty.is_float() {
                    Some(coerce_to_type(b, *arg, &target_ty))
                } else if is_complex_ty(&ty) {
                    // real(z) extracts the real component of a complex number.
                    // Complex values live as ptr<[f32/f64 x 2]>; load element 0.
                    let fw = complex_float_width(&ty);
                    let zero = b.const_i64(0);
                    let re_ptr = b.gep(*arg, vec![zero], IrType::Int(IntWidth::I8));
                    let real_part = b.load_typed(re_ptr, IrType::Float(fw));
                    Some(coerce_to_type(b, real_part, &target_ty))
                } else {
                    Some(*arg)
                }
            } else {
                None
            }
        }
        "logical" => {
            if let Some(arg) = args.first() {
                let requested_ty = args
                    .get(1)
                    .and_then(|kind| extract_const_int_from_value(b, *kind))
                    .and_then(|kind| match kind {
                        1 => Some(IrType::Int(IntWidth::I8)),
                        2 => Some(IrType::Int(IntWidth::I16)),
                        4 => Some(IrType::Bool),
                        8 => Some(IrType::Int(IntWidth::I64)),
                        _ => None,
                    })
                    .unwrap_or(IrType::Bool);
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Bool);
                match ty {
                    IrType::Bool | IrType::Int(_) => Some(coerce_to_type(b, *arg, &requested_ty)),
                    _ => None,
                }
            } else {
                None
            }
        }
        "aimag" | "dimag" => {
            // aimag(z) extracts the imaginary component of a complex number.
            // Complex values live as ptr<[f32/f64 x 2]>; load element 1 at
            // byte offset 4 (f32) or 8 (f64).
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if is_complex_ty(&ty) {
                    let fw = complex_float_width(&ty);
                    let offset = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                    let im_ptr = b.gep(*arg, vec![offset], IrType::Int(IntWidth::I8));
                    Some(b.load_typed(im_ptr, IrType::Float(fw)))
                } else {
                    None
                }
            } else {
                None
            }
        }
        "dble" | "dfloat" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_int() {
                    Some(b.int_to_float(*arg, FloatWidth::F64))
                } else if matches!(ty, IrType::Float(FloatWidth::F32)) {
                    Some(b.float_extend(*arg, FloatWidth::F64))
                } else {
                    Some(*arg)
                }
            } else {
                None
            }
        }
        "max" | "max0" | "amax1" | "dmax1" => {
            if args.len() >= 2 {
                let mut ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_float() {
                    if args.iter().any(|arg| {
                        matches!(
                            b.func().value_type(*arg),
                            Some(IrType::Float(FloatWidth::F64))
                        )
                    }) {
                        ty = IrType::Float(FloatWidth::F64);
                    }
                } else if ty.is_int() {
                    let width = args
                        .iter()
                        .filter_map(|arg| b.func().value_type(*arg).and_then(|ty| ty.int_width()))
                        .max_by_key(|width| width.bits())
                        .unwrap_or(IntWidth::I32);
                    ty = IrType::Int(width);
                }
                let coerced: Vec<ValueId> = args
                    .iter()
                    .map(|arg| coerce_to_type(b, *arg, &ty))
                    .collect();
                let cmp = if ty.is_float() {
                    b.fcmp(CmpOp::Ge, coerced[0], coerced[1])
                } else {
                    b.icmp(CmpOp::Ge, coerced[0], coerced[1])
                };
                let mut result = b.select(cmp, coerced[0], coerced[1]);
                // Variadic: max(a, b, c, ...) chains.
                for arg in &coerced[2..] {
                    let cmp = if ty.is_float() {
                        b.fcmp(CmpOp::Ge, result, *arg)
                    } else {
                        b.icmp(CmpOp::Ge, result, *arg)
                    };
                    result = b.select(cmp, result, *arg);
                }
                Some(result)
            } else {
                None
            }
        }
        "min" | "min0" | "amin1" | "dmin1" => {
            if args.len() >= 2 {
                let mut ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_float() {
                    if args.iter().any(|arg| {
                        matches!(
                            b.func().value_type(*arg),
                            Some(IrType::Float(FloatWidth::F64))
                        )
                    }) {
                        ty = IrType::Float(FloatWidth::F64);
                    }
                } else if ty.is_int() {
                    let width = args
                        .iter()
                        .filter_map(|arg| b.func().value_type(*arg).and_then(|ty| ty.int_width()))
                        .max_by_key(|width| width.bits())
                        .unwrap_or(IntWidth::I32);
                    ty = IrType::Int(width);
                }
                let coerced: Vec<ValueId> = args
                    .iter()
                    .map(|arg| coerce_to_type(b, *arg, &ty))
                    .collect();
                let cmp = if ty.is_float() {
                    b.fcmp(CmpOp::Le, coerced[0], coerced[1])
                } else {
                    b.icmp(CmpOp::Le, coerced[0], coerced[1])
                };
                let mut result = b.select(cmp, coerced[0], coerced[1]);
                for arg in &coerced[2..] {
                    let cmp = if ty.is_float() {
                        b.fcmp(CmpOp::Le, result, *arg)
                    } else {
                        b.icmp(CmpOp::Le, result, *arg)
                    };
                    result = b.select(cmp, result, *arg);
                }
                Some(result)
            } else {
                None
            }
        }
        "sign" | "dsign" | "isign" => {
            // sign(a, b) = abs(a) * sign_of(b) = b >= 0 ? abs(a) : -abs(a)
            if args.len() >= 2 {
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let abs_a = if ty.is_float() {
                    b.fabs(args[0])
                } else {
                    let zero = match &ty {
                        IrType::Int(IntWidth::I64) => b.const_i64(0),
                        _ => b.const_i32(0),
                    };
                    let is_pos = b.icmp(CmpOp::Ge, args[0], zero);
                    let neg = b.ineg(args[0]);
                    b.select(is_pos, args[0], neg)
                };
                let neg_abs = if ty.is_float() {
                    b.fneg(abs_a)
                } else {
                    b.ineg(abs_a)
                };
                let zero = match &ty {
                    IrType::Float(FloatWidth::F32) => b.const_f32(0.0),
                    IrType::Float(_) => b.const_f64(0.0),
                    IrType::Int(IntWidth::I64) => b.const_i64(0),
                    _ => b.const_i32(0),
                };
                let b_pos = if ty.is_float() {
                    b.fcmp(CmpOp::Ge, args[1], zero)
                } else {
                    b.icmp(CmpOp::Ge, args[1], zero)
                };
                Some(b.select(b_pos, abs_a, neg_abs))
            } else {
                None
            }
        }
        "sqrt" | "dsqrt" => args.first().map(|a| b.fsqrt(*a)),
        // ---- Bit manipulation (inline) ----
        // Mixed-kind bit ops (e.g. iand(c_long, c_int)) must unify
        // widths to the wider operand before the IR-level bit_and,
        // or the verifier rejects "operand width mismatch". F2018
        // §16.9.104 doesn't require same kinds; gfortran silently
        // promotes. Audit31 Finding 14.
        "iand" => {
            if args.len() >= 2 {
                let (l, r) = unify_int_widths(b, args[0], args[1]);
                Some(b.bit_and(l, r))
            } else {
                None
            }
        }
        "ior" => {
            if args.len() >= 2 {
                let (l, r) = unify_int_widths(b, args[0], args[1]);
                Some(b.bit_or(l, r))
            } else {
                None
            }
        }
        "ieor" => {
            if args.len() >= 2 {
                let (l, r) = unify_int_widths(b, args[0], args[1]);
                Some(b.bit_xor(l, r))
            } else {
                None
            }
        }
        "not" => args.first().map(|a| b.bit_not(*a)),
        "leadz" => args.first().map(|a| {
            let value_width = int_width_of_value(b, *a).unwrap_or(IntWidth::I32);
            match value_width {
                IntWidth::I8 | IntWidth::I16 => {
                    let bits = small_int_bits_as_i32(b, *a, value_width);
                    let raw = b.clz(bits);
                    let adjust = b.const_i32((32 - value_width.bits()) as i32);
                    b.isub(raw, adjust)
                }
                IntWidth::I64 => {
                    let raw = b.clz(*a);
                    b.int_trunc(raw, IntWidth::I32)
                }
                _ => b.clz(*a),
            }
        }),
        "trailz" => args.first().map(|a| {
            let value_width = int_width_of_value(b, *a).unwrap_or(IntWidth::I32);
            match value_width {
                IntWidth::I8 | IntWidth::I16 => {
                    let bits = small_int_bits_as_i32(b, *a, value_width);
                    let raw = b.ctz(bits);
                    let zero = b.const_i32(0);
                    let width = b.const_i32(value_width.bits() as i32);
                    let is_zero = b.icmp(CmpOp::Eq, bits, zero);
                    b.select(is_zero, width, raw)
                }
                IntWidth::I64 => {
                    let raw = b.ctz(*a);
                    b.int_trunc(raw, IntWidth::I32)
                }
                _ => b.ctz(*a),
            }
        }),
        "popcount" | "popcnt" => {
            // Use __builtin_popcountll via runtime call since ARM64 NEON popcount
            // requires a complex instruction sequence.
            args.first().map(|a| {
                let value_width = int_width_of_value(b, *a).unwrap_or(IntWidth::I32);
                let bits = unsigned_bit_value_for_width(b, *a, value_width);
                let widened = b.int_extend(bits, IntWidth::I64, false);
                b.call(
                    FuncRef::External("afs_popcount".into()),
                    vec![widened],
                    IrType::Int(IntWidth::I32),
                )
            })
        }
        "ishft" => {
            // F2018 §16.9.95: ISHFT does a *logical* shift on the bit
            // representation of the integer. For int8/int16 values
            // already sign-extended into the 32-bit AArch64 register
            // (e.g. -32767_int16 = 0x8001 lives as 0xFFFF8001 in
            // w-reg), `lshr` would shift the upper sign-fill bits in
            // alongside, producing 0x00FFFF80 instead of 0x0080.
            // Mask args[0] to the kind's width before the shift so
            // the logical-right shift sees the unsigned bit pattern.
            if args.len() >= 2 {
                let shift_cmp_width = int_width_of_value(b, args[1]).unwrap_or(IntWidth::I32);
                let zero = int_const_for_width(b, shift_cmp_width, 0);
                let is_left = b.icmp(CmpOp::Ge, args[1], zero);
                let neg_shift = b.ineg(args[1]);
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let shift = coerce_int_like_to_width(b, args[1], value_width);
                let neg_shift = coerce_int_like_to_width(b, neg_shift, value_width);
                let right = match value_width {
                    IntWidth::I8 | IntWidth::I16 => {
                        let bits = small_int_bits_as_i32(b, args[0], value_width);
                        let shift32 = coerce_int_like_to_width(b, neg_shift, IntWidth::I32);
                        let shifted = b.lshr(bits, shift32);
                        b.int_trunc(shifted, value_width)
                    }
                    _ => b.lshr(args[0], neg_shift),
                };
                let left = b.shl(args[0], shift);
                Some(b.select(is_left, left, right))
            } else {
                None
            }
        }
        "shiftl" => {
            // F2008 §13.7.150: logical left shift, shift>=0.
            if args.len() >= 2 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let shift = coerce_int_like_to_width(b, args[1], value_width);
                Some(b.shl(args[0], shift))
            } else {
                None
            }
        }
        "shiftr" => {
            // F2008 §13.7.151: logical right shift (zero fill).
            if args.len() >= 2 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let value = unsigned_bit_value_for_width(b, args[0], value_width);
                let shift_width = match value_width {
                    IntWidth::I8 | IntWidth::I16 => IntWidth::I32,
                    _ => value_width,
                };
                let shift = coerce_int_like_to_width(b, args[1], shift_width);
                let shifted = b.lshr(value, shift);
                Some(truncate_bit_value_to_width(b, shifted, value_width))
            } else {
                None
            }
        }
        "shifta" => {
            // F2008 §13.7.149: arithmetic right shift (sign-extending).
            // No native ashr in the IR yet — synthesize as
            //   shifta(x, n) = lshr(x, n) | (sign_mask << (width - n))
            // where sign_mask is all-ones if MSB(x) is set, else 0.
            if args.len() >= 2 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let shift = coerce_int_like_to_width(b, args[1], value_width);
                let logical = b.lshr(args[0], shift);
                let bits = int_const_for_width(b, value_width, value_width.bits() as i64);
                let pos_top = b.isub(bits, shift);
                // Pre-fill: -1 if top bit of x is 1, else 0.
                let one = int_const_for_width(b, value_width, 1);
                let top_bit_pos =
                    int_const_for_width(b, value_width, (value_width.bits() - 1) as i64);
                let top_bit = b.lshr(args[0], top_bit_pos);
                let sign = b.bit_and(top_bit, one);
                let neg_one = int_const_for_width(b, value_width, -1);
                let zero = int_const_for_width(b, value_width, 0);
                let is_neg = b.icmp(CmpOp::Ne, sign, zero);
                let mask_full = b.select(is_neg, neg_one, zero);
                let high_mask = b.shl(mask_full, pos_top);
                Some(b.bit_or(logical, high_mask))
            } else {
                None
            }
        }
        "btest" => {
            // btest(a, pos) = (a >> pos) & 1 /= 0
            if args.len() >= 2 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let value = unsigned_bit_value_for_width(b, args[0], value_width);
                let op_width = match value_width {
                    IntWidth::I8 | IntWidth::I16 => IntWidth::I32,
                    _ => value_width,
                };
                let pos = coerce_int_like_to_width(b, args[1], op_width);
                let shifted = b.lshr(value, pos);
                let one = int_const_for_width(b, op_width, 1);
                let masked = b.bit_and(shifted, one);
                let zero = int_const_for_width(b, op_width, 0);
                Some(b.icmp(CmpOp::Ne, masked, zero))
            } else {
                None
            }
        }
        "ibset" => {
            // ibset(a, pos) = a | (1 << pos)
            if args.len() >= 2 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let one = int_const_for_width(b, value_width, 1);
                let pos = coerce_int_like_to_width(b, args[1], value_width);
                let mask = b.shl(one, pos);
                Some(b.bit_or(args[0], mask))
            } else {
                None
            }
        }
        "ibclr" => {
            // ibclr(a, pos) = a & ~(1 << pos)
            if args.len() >= 2 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let one = int_const_for_width(b, value_width, 1);
                let pos = coerce_int_like_to_width(b, args[1], value_width);
                let mask = b.shl(one, pos);
                let inv = b.bit_not(mask);
                Some(b.bit_and(args[0], inv))
            } else {
                None
            }
        }
        "ibits" => {
            // ibits(i, pos, len) = (i >> pos) & ((1 << len) - 1)
            if args.len() >= 3 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let value = unsigned_bit_value_for_width(b, args[0], value_width);
                let op_width = match value_width {
                    IntWidth::I8 | IntWidth::I16 => IntWidth::I32,
                    _ => value_width,
                };
                let pos = coerce_int_like_to_width(b, args[1], op_width);
                let len = coerce_int_like_to_width(b, args[2], op_width);
                let shifted = b.lshr(value, pos);
                let one = int_const_for_width(b, op_width, 1);
                let mask_hi = b.shl(one, len);
                let one2 = int_const_for_width(b, op_width, 1);
                let mask = b.isub(mask_hi, one2);
                let result = b.bit_and(shifted, mask);
                Some(truncate_bit_value_to_width(b, result, value_width))
            } else {
                None
            }
        }
        // F2018 §16.9.21-24 — unsigned bitwise comparisons.  Implemented
        // via the "flip the sign bit, then signed compare" trick so the
        // existing signed icmp ops produce unsigned ordering.
        "bge" | "bgt" | "ble" | "blt" => {
            if args.len() >= 2 {
                let (l, r) = unify_int_widths(b, args[0], args[1]);
                let value_width = int_width_of_value(b, l).unwrap_or(IntWidth::I32);
                let sign_bit_pos =
                    int_const_for_width(b, value_width, (value_width.bits() - 1) as i64);
                let one = int_const_for_width(b, value_width, 1);
                let sign_mask = b.shl(one, sign_bit_pos);
                let l_flipped = b.bit_xor(l, sign_mask);
                let r_flipped = b.bit_xor(r, sign_mask);
                let op = match name {
                    "bge" => CmpOp::Ge,
                    "bgt" => CmpOp::Gt,
                    "ble" => CmpOp::Le,
                    "blt" => CmpOp::Lt,
                    _ => unreachable!(),
                };
                Some(b.icmp(op, l_flipped, r_flipped))
            } else {
                None
            }
        }
        "poppar" => {
            // F2018 §16.9.179: 1 if popcount(i) is odd, 0 otherwise.
            args.first().map(|a| {
                let widened = b.int_extend(*a, IntWidth::I64, false);
                let cnt = b.call(
                    FuncRef::External("afs_popcount".into()),
                    vec![widened],
                    IrType::Int(IntWidth::I32),
                );
                let one = b.const_i32(1);
                b.bit_and(cnt, one)
            })
        }
        "merge_bits" => {
            // F2018 §16.9.150: (i AND mask) IOR (j AND NOT mask)
            if args.len() >= 3 {
                let (i, j) = unify_int_widths(b, args[0], args[1]);
                let value_width = int_width_of_value(b, i).unwrap_or(IntWidth::I32);
                let mask = coerce_int_like_to_width(b, args[2], value_width);
                let lhs = b.bit_and(i, mask);
                let inv_mask = b.bit_not(mask);
                let rhs = b.bit_and(j, inv_mask);
                Some(b.bit_or(lhs, rhs))
            } else {
                None
            }
        }
        "maskl" => {
            // F2018 §16.9.139: i leftmost bits set; rest cleared.
            // maskl(i) = (-1) << (bits - i) if i > 0, else 0.
            args.first().map(|a| {
                let value_width = int_width_of_value(b, *a).unwrap_or(IntWidth::I32);
                let bits = int_const_for_width(b, value_width, value_width.bits() as i64);
                let i_in_w = coerce_int_like_to_width(b, *a, value_width);
                let shift = b.isub(bits, i_in_w);
                let neg_one = int_const_for_width(b, value_width, -1);
                let zero = int_const_for_width(b, value_width, 0);
                let shifted = b.shl(neg_one, shift);
                let is_zero = b.icmp(CmpOp::Le, i_in_w, zero);
                b.select(is_zero, zero, shifted)
            })
        }
        "maskr" => {
            // F2018 §16.9.140: i rightmost bits set; rest cleared.
            // maskr(i) = (1 << i) - 1 for 0 < i < bits; 0 for i==0; -1 for i>=bits.
            args.first().map(|a| {
                let value_width = int_width_of_value(b, *a).unwrap_or(IntWidth::I32);
                let one = int_const_for_width(b, value_width, 1);
                let i_in_w = coerce_int_like_to_width(b, *a, value_width);
                let shifted = b.shl(one, i_in_w);
                let one_again = int_const_for_width(b, value_width, 1);
                let computed = b.isub(shifted, one_again);
                let zero = int_const_for_width(b, value_width, 0);
                let bits = int_const_for_width(b, value_width, value_width.bits() as i64);
                let neg_one = int_const_for_width(b, value_width, -1);
                let too_big = b.icmp(CmpOp::Ge, i_in_w, bits);
                let is_zero = b.icmp(CmpOp::Le, i_in_w, zero);
                let big_or_normal = b.select(too_big, neg_one, computed);
                b.select(is_zero, zero, big_or_normal)
            })
        }
        "dshiftl" => {
            // F2018 §16.9.59: combine i and j as a 2*bits value with
            // i on the left, then logical-shift left by `shift` and
            // return the leftmost `bits` bits.
            //   dshiftl(i, j, s) = (i << s) | (j >> (bits - s))
            if args.len() >= 3 {
                let (i, j) = unify_int_widths(b, args[0], args[1]);
                let value_width = int_width_of_value(b, i).unwrap_or(IntWidth::I32);
                let shift = coerce_int_like_to_width(b, args[2], value_width);
                let bits = int_const_for_width(b, value_width, value_width.bits() as i64);
                let comp = b.isub(bits, shift);
                let left = b.shl(i, shift);
                let right = b.lshr(j, comp);
                Some(b.bit_or(left, right))
            } else {
                None
            }
        }
        "dshiftr" => {
            // F2018 §16.9.60: combine i and j and shift right by `shift`,
            // returning the rightmost `bits` bits.
            //   dshiftr(i, j, s) = (j >> s) | (i << (bits - s))
            if args.len() >= 3 {
                let (i, j) = unify_int_widths(b, args[0], args[1]);
                let value_width = int_width_of_value(b, i).unwrap_or(IntWidth::I32);
                let shift = coerce_int_like_to_width(b, args[2], value_width);
                let bits = int_const_for_width(b, value_width, value_width.bits() as i64);
                let comp = b.isub(bits, shift);
                let right = b.lshr(j, shift);
                let left = b.shl(i, comp);
                Some(b.bit_or(left, right))
            } else {
                None
            }
        }
        // ---- Math intrinsics → libm calls ----
        // Dispatch to sinf/sin based on argument type for F32/F64 correctness.
        "sin" | "dsin" | "cos" | "dcos" | "tan" | "dtan" | "asin" | "dasin" | "acos" | "dacos"
        | "atan" | "datan" | "sinh" | "dsinh" | "cosh" | "dcosh" | "tanh" | "dtanh" | "asinh"
        | "acosh" | "atanh" | "exp" | "dexp" | "log" | "dlog" | "alog" | "log10" | "dlog10"
        | "alog10" | "erf" | "derf" | "erfc" | "derfc" | "ceiling" | "floor" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let base_name = match name {
                    "dsin" | "sin" => "sin",
                    "dcos" | "cos" => "cos",
                    "dtan" | "tan" => "tan",
                    "dasin" | "asin" => "asin",
                    "dacos" | "acos" => "acos",
                    "datan" | "atan" => "atan",
                    "dsinh" | "sinh" => "sinh",
                    "dcosh" | "cosh" => "cosh",
                    "dtanh" | "tanh" => "tanh",
                    "asinh" => "asinh",
                    "acosh" => "acosh",
                    "atanh" => "atanh",
                    "dexp" | "exp" => "exp",
                    "dlog" | "log" | "alog" => "log",
                    "dlog10" | "log10" | "alog10" => "log10",
                    "derf" | "erf" => "erf",
                    "derfc" | "erfc" => "erfc",
                    "ceiling" => "ceil",
                    "floor" => "floor",
                    _ => name,
                };
                let func_name = if is_f32 {
                    format!("{}f", base_name)
                } else {
                    base_name.to_string()
                };
                let ret_ty = if is_f32 {
                    IrType::Float(FloatWidth::F32)
                } else {
                    IrType::Float(FloatWidth::F64)
                };
                Some(b.call(FuncRef::External(func_name), vec![*arg], ret_ty))
            } else {
                None
            }
        }
        "atan2" | "datan2" => {
            if args.len() >= 2 {
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let func = if is_f32 { "atan2f" } else { "atan2" };
                let ret_ty = if is_f32 {
                    IrType::Float(FloatWidth::F32)
                } else {
                    IrType::Float(FloatWidth::F64)
                };
                Some(b.call(
                    FuncRef::External(func.into()),
                    vec![args[0], args[1]],
                    ret_ty,
                ))
            } else {
                None
            }
        }
        // F2023 degree / half-revolution trig → runtime (afs_*), not
        // libm: the exactness contract needs reduction in the native
        // unit (see runtime/src/math.rs). One-argument forms.
        "acosd" | "asind" | "atand" | "cosd" | "sind" | "tand" | "acospi" | "asinpi" | "atanpi"
        | "cospi" | "sinpi" | "tanpi" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let base = format!("afs_{}", name);
                let func_name = if is_f32 {
                    format!("{}_f32", base)
                } else {
                    base
                };
                let ret_ty = if is_f32 {
                    IrType::Float(FloatWidth::F32)
                } else {
                    IrType::Float(FloatWidth::F64)
                };
                Some(b.call(FuncRef::External(func_name), vec![*arg], ret_ty))
            } else {
                None
            }
        }
        "atan2d" | "atan2pi" => {
            if args.len() >= 2 {
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let base = format!("afs_{}", name);
                let func_name = if is_f32 {
                    format!("{}_f32", base)
                } else {
                    base
                };
                let ret_ty = if is_f32 {
                    IrType::Float(FloatWidth::F32)
                } else {
                    IrType::Float(FloatWidth::F64)
                };
                Some(b.call(FuncRef::External(func_name), vec![args[0], args[1]], ret_ty))
            } else {
                None
            }
        }
        "gamma" | "dgamma" => args.first().map(|a| {
            let ty = b
                .func()
                .value_type(*a)
                .unwrap_or(IrType::Float(FloatWidth::F64));
            let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
            let func = if is_f32 { "tgammaf" } else { "tgamma" };
            let ret_ty = if is_f32 {
                IrType::Float(FloatWidth::F32)
            } else {
                IrType::Float(FloatWidth::F64)
            };
            b.call(FuncRef::External(func.into()), vec![*a], ret_ty)
        }),
        "log_gamma" => args.first().map(|a| {
            let ty = b
                .func()
                .value_type(*a)
                .unwrap_or(IrType::Float(FloatWidth::F64));
            let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
            let func = if is_f32 { "lgammaf" } else { "lgamma" };
            let ret_ty = if is_f32 {
                IrType::Float(FloatWidth::F32)
            } else {
                IrType::Float(FloatWidth::F64)
            };
            b.call(FuncRef::External(func.into()), vec![*a], ret_ty)
        }),
        "bessel_j0" => args.first().map(|a| {
            b.call(
                FuncRef::External("j0".into()),
                vec![*a],
                IrType::Float(FloatWidth::F64),
            )
        }),
        "bessel_j1" => args.first().map(|a| {
            b.call(
                FuncRef::External("j1".into()),
                vec![*a],
                IrType::Float(FloatWidth::F64),
            )
        }),
        "bessel_y0" => args.first().map(|a| {
            b.call(
                FuncRef::External("y0".into()),
                vec![*a],
                IrType::Float(FloatWidth::F64),
            )
        }),
        "bessel_y1" => args.first().map(|a| {
            b.call(
                FuncRef::External("y1".into()),
                vec![*a],
                IrType::Float(FloatWidth::F64),
            )
        }),
        "hypot" => {
            if args.len() >= 2 {
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let func = if is_f32 { "hypotf" } else { "hypot" };
                let ret_ty = if is_f32 {
                    IrType::Float(FloatWidth::F32)
                } else {
                    IrType::Float(FloatWidth::F64)
                };
                Some(b.call(
                    FuncRef::External(func.into()),
                    vec![args[0], args[1]],
                    ret_ty,
                ))
            } else {
                None
            }
        }
        "ishftc" => {
            // ishftc(a, shift, size): circular shift of the rightmost `size` bits.
            // F2018 §16.9.108. The previous implementation used
            //   mask = (1 << size) - 1
            // which is undefined when size equals the operand bit width
            // (AArch64 LSL masks the shift amount mod 64, so 1 << 64 wraps
            // back to 1, leaving mask = 0 and erasing the result).
            // stdlib_random's xoshiro256ss called ishftc(x, 7) with the
            // default size = 64 and got 0 every time.
            if args.len() >= 2 {
                let value_width = int_width_of_value(b, args[0]).unwrap_or(IntWidth::I32);
                let bit_width = match value_width {
                    IntWidth::I64 => 64,
                    IntWidth::I16 => 16,
                    IntWidth::I8 => 8,
                    _ => 32,
                };
                let shift = coerce_int_like_to_width(b, args[1], value_width);
                let bw_minus_1 = int_const_for_width(b, value_width, bit_width - 1);
                if args.len() >= 3 {
                    // Explicit size: rotate within the rightmost `size` bits,
                    // leaving the upper (bit_width - size) bits untouched.
                    let size = coerce_int_like_to_width(b, args[2], value_width);
                    // Build mask = ((1 << (size-1)) - 1) << 1 | 1.
                    // Valid for size in [1, bit_width]; at size == bit_width
                    // this yields all ones without ever shifting by bit_width.
                    let one_a = int_const_for_width(b, value_width, 1);
                    let one_b = int_const_for_width(b, value_width, 1);
                    let one_c = int_const_for_width(b, value_width, 1);
                    let one_d = int_const_for_width(b, value_width, 1);
                    let size_minus_1 = b.isub(size, one_a);
                    let half = b.shl(one_b, size_minus_1);
                    let half_minus_1 = b.isub(half, one_c);
                    let half_minus_1_shifted = b.shl(half_minus_1, one_d);
                    let one_e = int_const_for_width(b, value_width, 1);
                    let mask = b.bit_or(half_minus_1_shifted, one_e);
                    let not_mask_pre = int_const_for_width(b, value_width, -1);
                    let not_mask = b.bit_xor(not_mask_pre, mask);
                    // Rotate the low bits, preserve the high bits.
                    let low = b.bit_and(args[0], mask);
                    let high = b.bit_and(args[0], not_mask);
                    // shift_safe = shift mod size (avoid UB when shift == size).
                    // For the common stdlib usage shift < size, so the modulo is
                    // a no-op; we still emit isub-based fallback in case.
                    let left_pre = b.shl(low, shift);
                    let left = b.bit_and(left_pre, mask);
                    let diff = b.isub(size, shift);
                    let right = b.lshr(low, diff);
                    let rotated = b.bit_or(left, right);
                    let rotated_low = b.bit_and(rotated, mask);
                    Some(b.bit_or(rotated_low, high))
                } else {
                    // Default size: full-width rotate. Use the standard
                    //   (a << (s & (BITS-1))) | (a >> ((-s) & (BITS-1)))
                    // formula which is well-defined for shift = 0 because
                    // both lanes shift by 0 and OR back the same value.
                    let s_masked = b.bit_and(shift, bw_minus_1);
                    let bw_const = int_const_for_width(b, value_width, bit_width);
                    let neg_s = b.isub(bw_const, s_masked);
                    let bw_minus_1_b = int_const_for_width(b, value_width, bit_width - 1);
                    let neg_s_masked = b.bit_and(neg_s, bw_minus_1_b);
                    let left = b.shl(args[0], s_masked);
                    let right = b.lshr(args[0], neg_s_masked);
                    Some(b.bit_or(left, right))
                }
            } else {
                None
            }
        }

        // ---- Numeric inquiry intrinsics (compile-time constants) ----
        // These depend on the argument's type, which we determine from the first arg.
        "huge" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                match &ty {
                    IrType::Int(IntWidth::I8) => Some(b.const_i32(i8::MAX as i64 as i32)),
                    IrType::Int(IntWidth::I16) => Some(b.const_i32(i16::MAX as i64 as i32)),
                    IrType::Int(IntWidth::I32) => Some(b.const_i32(i32::MAX)),
                    IrType::Int(IntWidth::I64) => Some(b.const_i64(i64::MAX)),
                    IrType::Float(FloatWidth::F32) => Some(b.const_f32(f32::MAX)),
                    IrType::Float(FloatWidth::F64) => Some(b.const_f64(f64::MAX)),
                    _ => None,
                }
            } else {
                None
            }
        }
        "tiny" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Float(FloatWidth::F32));
                match &ty {
                    IrType::Float(FloatWidth::F32) => Some(b.const_f32(f32::MIN_POSITIVE)),
                    IrType::Float(FloatWidth::F64) => Some(b.const_f64(f64::MIN_POSITIVE)),
                    _ => None,
                }
            } else {
                None
            }
        }
        "epsilon" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Float(FloatWidth::F32));
                match &ty {
                    IrType::Float(FloatWidth::F32) => Some(b.const_f32(f32::EPSILON)),
                    IrType::Float(FloatWidth::F64) => Some(b.const_f64(f64::EPSILON)),
                    _ => None,
                }
            } else {
                None
            }
        }
        "precision" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Float(FloatWidth::F32));
                let prec = match &ty {
                    IrType::Float(FloatWidth::F32) => 6,  // ~7.2 decimal digits → 6
                    IrType::Float(FloatWidth::F64) => 15, // ~15.9 decimal digits → 15
                    _ => 0,
                };
                Some(b.const_i32(prec))
            } else {
                None
            }
        }
        "range" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let range = match &ty {
                    IrType::Int(IntWidth::I8) => 2,
                    IrType::Int(IntWidth::I16) => 4,
                    IrType::Int(IntWidth::I32) => 9,
                    IrType::Int(IntWidth::I64) => 18,
                    IrType::Int(IntWidth::I128) => 38,
                    IrType::Float(FloatWidth::F32) => 37,
                    IrType::Float(FloatWidth::F64) => 307,
                    _ => 0,
                };
                Some(b.const_i32(range))
            } else {
                None
            }
        }
        "digits" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let digits = match &ty {
                    IrType::Int(IntWidth::I8) => 7,
                    IrType::Int(IntWidth::I16) => 15,
                    IrType::Int(IntWidth::I32) => 31,
                    IrType::Int(IntWidth::I64) => 63,
                    IrType::Int(IntWidth::I128) => 127,
                    IrType::Float(FloatWidth::F32) => 24, // significand bits
                    IrType::Float(FloatWidth::F64) => 53,
                    _ => 0,
                };
                Some(b.const_i32(digits))
            } else {
                None
            }
        }
        "radix" => {
            // Always 2 for binary machines.
            Some(b.const_i32(2))
        }
        "bit_size" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let bits = match &ty {
                    IrType::Int(IntWidth::I8) => 8,
                    IrType::Int(IntWidth::I16) => 16,
                    IrType::Int(IntWidth::I32) => 32,
                    IrType::Int(IntWidth::I64) => 64,
                    IrType::Int(IntWidth::I128) => 128,
                    _ => 0,
                };
                Some(b.const_i32(bits))
            } else {
                None
            }
        }
        // F2018 §16.9.196: STORAGE_SIZE(A [, KIND]) returns the size in
        // bits a value of the same type as A occupies. For non-polymorphic
        // arguments this is determined entirely by the argument's IR type
        // and can be folded at compile time (8 * sizeof). The optional
        // KIND argument names the kind of the result; we always return I32
        // and let downstream coerce.
        "storage_size" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let bits = storage_size_bits_for_ir_type(&ty);
                Some(b.const_i32(bits))
            } else {
                None
            }
        }
        "kind" => {
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let kind = match &ty {
                    IrType::Int(IntWidth::I8) => 1,
                    IrType::Int(IntWidth::I16) => 2,
                    IrType::Int(IntWidth::I32) => 4,
                    IrType::Int(IntWidth::I64) => 8,
                    IrType::Int(IntWidth::I128) => 16,
                    IrType::Float(FloatWidth::F32) => 4,
                    IrType::Float(FloatWidth::F64) => 8,
                    IrType::Bool => 4,
                    _ => 4,
                };
                Some(b.const_i32(kind))
            } else {
                None
            }
        }
        // ---- System inquiry functions ----
        "command_argument_count" => Some(b.call(
            FuncRef::External("afs_command_argument_count".into()),
            vec![],
            IrType::Int(IntWidth::I32),
        )),
        "is_iostat_end" => args.first().map(|status| {
            let zero = match b
                .func()
                .value_type(*status)
                .unwrap_or(IrType::Int(IntWidth::I32))
            {
                IrType::Int(IntWidth::I64) => b.const_i64(-1),
                _ => b.const_i32(-1),
            };
            b.icmp(CmpOp::Eq, *status, zero)
        }),
        "is_iostat_eor" => args.first().map(|status| {
            let zero = match b
                .func()
                .value_type(*status)
                .unwrap_or(IrType::Int(IntWidth::I32))
            {
                IrType::Int(IntWidth::I64) => b.const_i64(-2),
                _ => b.const_i32(-2),
            };
            b.icmp(CmpOp::Eq, *status, zero)
        }),

        // ---- iso_c_binding functions ----
        "c_loc" => None,
        "c_sizeof" => {
            // c_sizeof(x) — return byte size of x's C representation.
            if let Some(arg) = args.first() {
                let ty = b
                    .func()
                    .value_type(*arg)
                    .unwrap_or(IrType::Int(IntWidth::I32));
                let size: i64 = match &ty {
                    IrType::Int(IntWidth::I8) | IrType::Bool => 1,
                    IrType::Int(IntWidth::I16) => 2,
                    IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32) => 4,
                    IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => 8,
                    IrType::Int(IntWidth::I128) => 16,
                    IrType::Ptr(_) => b.layout.ptr_bytes as i64,
                    // Arrays use element size * count, but we don't have shape info here.
                    // For now, return element size. Proper impl needs descriptor access.
                    IrType::Array(elem, count) => {
                        let elem_size = ir_scalar_byte_size(elem.as_ref(), b.layout);
                        elem_size * (*count as i64)
                    }
                    _ => 8, // default to pointer size for unknown types
                };
                Some(b.const_i64(size))
            } else {
                None
            }
        }
        "c_associated" => {
            // c_associated(p) → p /= null
            // c_associated(p, q) → p == q
            if args.len() >= 2 {
                Some(b.icmp(CmpOp::Eq, args[0], args[1]))
            } else if let Some(p) = args.first() {
                // Use type-matched zero to avoid register width mismatch.
                let ty = b
                    .func()
                    .value_type(*p)
                    .unwrap_or(IrType::Int(IntWidth::I64));
                let null = match &ty {
                    IrType::Int(IntWidth::I32) => b.const_i32(0),
                    _ => b.const_i64(0),
                };
                Some(b.icmp(CmpOp::Ne, *p, null))
            } else {
                None
            }
        }

        // ---- Kind selection intrinsics ----
        "selected_int_kind" => {
            // selected_int_kind(r): smallest integer kind whose range covers [-10^r, 10^r].
            if let Some(arg) = args.first() {
                if let Some(r) = extract_const_int_from_value(b, *arg) {
                    let kind: i32 = if r <= 2 {
                        1
                    }
                    // i8: ±127
                    else if r <= 4 {
                        2
                    }
                    // i16: ±32767
                    else if r <= 9 {
                        4
                    }
                    // i32: ±2.1e9
                    else if r <= 18 {
                        8
                    }
                    // i64: ±9.2e18
                    else if r <= 38 {
                        16
                    }
                    // i128: ±1.7e38
                    else {
                        -1
                    }; // no kind available
                    Some(b.const_i32(kind))
                } else {
                    Some(b.const_i32(4)) // non-constant: default to 4
                }
            } else {
                None
            }
        }
        "selected_real_kind" => {
            // selected_real_kind(p[, r]): smallest real kind with ≥p decimal digits.
            if let Some(arg) = args.first() {
                if let Some(p) = extract_const_int_from_value(b, *arg) {
                    let kind: i32 = if p <= 6 {
                        4
                    }
                    // f32: ~7 digits
                    else if p <= 15 {
                        8
                    }
                    // f64: ~15 digits
                    else {
                        -1
                    }; // no kind available
                    Some(b.const_i32(kind))
                } else {
                    Some(b.const_i32(8)) // non-constant: default to 8
                }
            } else {
                None
            }
        }
        "selected_logical_kind" => {
            // SELECTED_LOGICAL_KIND(BITS): smallest logical kind whose
            // storage is at least BITS bits (1/2/4/8 → 8/16/32/64 bits),
            // -1 if none. Const-foldable here; non-constant args fall to
            // the runtime helper (the kind set is fixed, no libm need).
            if let Some(arg) = args.first() {
                if let Some(bits) = extract_const_int_from_value(b, *arg) {
                    let kind: i32 = if bits <= 8 {
                        1
                    } else if bits <= 16 {
                        2
                    } else if bits <= 32 {
                        4
                    } else if bits <= 64 {
                        8
                    } else {
                        -1
                    };
                    Some(b.const_i32(kind))
                } else {
                    let v = coerce_int_like_to_width(b, *arg, IntWidth::I32);
                    Some(b.call(
                        FuncRef::External("afs_selected_logical_kind".into()),
                        vec![v],
                        IrType::Int(IntWidth::I32),
                    ))
                }
            } else {
                None
            }
        }

        // ---- IEEE arithmetic intrinsics ----
        // Predicates and classification go through runtime bit-pattern
        // helpers (`runtime/src/ieee.rs`) rather than compare-based IR: a
        // call is opaque to const folding, so `x /= x`-style identities
        // that passes rewrite to `.false.` can't break NaN detection.
        "ieee_is_nan" | "ieee_is_finite" | "ieee_is_normal" => {
            args.first().map(|arg| {
                let suffix = ieee_float_suffix(b, *arg);
                let op = match name {
                    "ieee_is_nan" => "is_nan",
                    "ieee_is_finite" => "is_finite",
                    _ => "is_normal",
                };
                let r = b.call(
                    FuncRef::External(format!("afs_ieee_{}_{}", op, suffix)),
                    vec![*arg],
                    IrType::Int(IntWidth::I32),
                );
                let zero = b.const_i32(0);
                b.icmp(CmpOp::Ne, r, zero)
            })
        }
        "ieee_unordered" => {
            if args.len() < 2 {
                None
            } else {
                let suffix = ieee_float_suffix(b, args[0]);
                let r = b.call(
                    FuncRef::External(format!("afs_ieee_unordered_{}", suffix)),
                    vec![args[0], args[1]],
                    IrType::Int(IntWidth::I32),
                );
                let zero = b.const_i32(0);
                Some(b.icmp(CmpOp::Ne, r, zero))
            }
        }
        "ieee_class" => args.first().map(|arg| {
            let suffix = ieee_float_suffix(b, *arg);
            b.call(
                FuncRef::External(format!("afs_ieee_class_{}", suffix)),
                vec![*arg],
                IrType::Int(IntWidth::I32),
            )
        }),
        "ieee_copy_sign" => {
            if args.len() < 2 {
                None
            } else {
                let suffix = ieee_float_suffix(b, args[0]);
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                Some(b.call(
                    FuncRef::External(format!("afs_ieee_copy_sign_{}", suffix)),
                    vec![args[0], args[1]],
                    ty,
                ))
            }
        }
        "ieee_logb" | "ieee_rint" => args.first().map(|arg| {
            let suffix = ieee_float_suffix(b, *arg);
            let op = if name == "ieee_logb" { "logb" } else { "rint" };
            let ty = b
                .func()
                .value_type(*arg)
                .unwrap_or(IrType::Float(FloatWidth::F64));
            b.call(
                FuncRef::External(format!("afs_ieee_{}_{}", op, suffix)),
                vec![*arg],
                ty,
            )
        }),
        "ieee_scalb" => {
            if args.len() < 2 {
                None
            } else {
                let suffix = ieee_float_suffix(b, args[0]);
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let i = ieee_as_i32(b, args[1]);
                Some(b.call(
                    FuncRef::External(format!("afs_ieee_scalb_{}", suffix)),
                    vec![args[0], i],
                    ty,
                ))
            }
        }
        "ieee_next_after" => {
            if args.len() < 2 {
                None
            } else {
                let suffix = ieee_float_suffix(b, args[0]);
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                Some(b.call(
                    FuncRef::External(format!("afs_ieee_next_after_{}", suffix)),
                    vec![args[0], args[1]],
                    ty,
                ))
            }
        }
        // F2023 / 60559:2020 maximum/minimum family. The runtime entry
        // point name is the intrinsic name with `ieee_`→`afs_ieee_` and
        // an r4/r8 suffix.
        "ieee_max" | "ieee_min" | "ieee_max_mag" | "ieee_min_mag" | "ieee_max_num"
        | "ieee_min_num" | "ieee_max_num_mag" | "ieee_min_num_mag" => {
            if args.len() < 2 {
                None
            } else {
                let suffix = ieee_float_suffix(b, args[0]);
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let op = &name["ieee_".len()..];
                Some(b.call(
                    FuncRef::External(format!("afs_ieee_{}_{}", op, suffix)),
                    vec![args[0], args[1]],
                    ty,
                ))
            }
        }
        // Honest support answers (l09 deliverable 1 matrix). True only for
        // what is implemented and tested; the rest say false so the
        // stdlib probe-before-use pattern routes around them.
        "ieee_support_datatype"
        | "ieee_support_denormal"
        | "ieee_support_inf"
        | "ieee_support_nan"
        | "ieee_support_subnormal"
        | "ieee_support_divide"
        | "ieee_support_sqrt"
        | "ieee_support_io"
        | "ieee_support_rounding"
        | "ieee_support_flag" => Some(b.const_bool(true)),
        "ieee_support_underflow_control"
        | "ieee_support_halting"
        | "ieee_support_standard" => Some(b.const_bool(false)),
        "maxexponent" => {
            // F2018 §16.9.124: returns the maximum exponent in the model
            // for the same kind as the argument. For IEEE binary32 = 128,
            // binary64 = 1024.
            let arg_ty = args
                .first()
                .and_then(|a| b.func().value_type(*a))
                .unwrap_or(IrType::Float(FloatWidth::F32));
            let val = match arg_ty {
                IrType::Float(FloatWidth::F64) => 1024_i32,
                _ => 128_i32,
            };
            Some(b.const_i32(val))
        }
        "minexponent" => {
            // F2018 §16.9.146: minimum exponent in the model. binary32 = -125,
            // binary64 = -1021.
            let arg_ty = args
                .first()
                .and_then(|a| b.func().value_type(*a))
                .unwrap_or(IrType::Float(FloatWidth::F32));
            let val = match arg_ty {
                IrType::Float(FloatWidth::F64) => -1021_i32,
                _ => -125_i32,
            };
            Some(b.const_i32(val))
        }
        "ieee_value" => {
            // IEEE_VALUE(X, CLASS): the value's KIND comes from X, the
            // value itself from CLASS. Going through the runtime keeps the
            // NaN/Inf results from being const-folded away at -Ofast
            // (`0.0/0.0` would fold to a non-signaling pattern or be
            // dropped); a call is opaque to folding.
            if args.len() < 2 {
                None
            } else {
                let suffix = ieee_float_suffix(b, args[0]);
                let ty = b
                    .func()
                    .value_type(args[0])
                    .unwrap_or(IrType::Float(FloatWidth::F64));
                let class = ieee_as_i32(b, args[1]);
                Some(b.call(
                    FuncRef::External(format!("afs_ieee_value_{}", suffix)),
                    vec![class],
                    ty,
                ))
            }
        }

        _ => None,
    }
}

/// `r4`/`r8` runtime-symbol suffix for an IEEE intrinsic argument, from
/// its IR float width (defaults to double).
fn ieee_float_suffix(b: &FuncBuilder, v: ValueId) -> &'static str {
    match b.func().value_type(v) {
        Some(IrType::Float(FloatWidth::F32)) => "r4",
        _ => "r8",
    }
}

/// Coerce an integer value to i32 for the `extern "C" fn(.., i32)` IEEE
/// runtime entry points. Wider integers truncate; i32 passes through.
fn ieee_as_i32(b: &mut FuncBuilder, v: ValueId) -> ValueId {
    match b.func().value_type(v) {
        Some(IrType::Int(IntWidth::I32)) => v,
        Some(IrType::Int(IntWidth::I64 | IntWidth::I128)) => b.int_trunc(v, IntWidth::I32),
        Some(IrType::Int(IntWidth::I8 | IntWidth::I16)) => b.int_extend(v, IntWidth::I32, true),
        _ => v,
    }
}
