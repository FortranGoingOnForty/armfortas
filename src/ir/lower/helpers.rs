//! Pure leaf helpers used throughout lowering.
//!
//! Type coercion, integer widening, and storage-size queries — small
//! self-contained utilities that don't carry any `LowerCtx` state.
//!
//! Extracted from `lower::core` in sprint 04 step 4. No behavior
//! change.

use crate::ir::builder::FuncBuilder;
use crate::ir::inst::{CmpOp, ValueId};
use crate::ir::types::{FloatWidth, IntWidth, IrType};

/// Coerce a scalar value to a target type for initializer storage.
///
/// Covers every Fortran scalar coercion that can show up at an
/// initializer-store site:
///   * Int → Int width change (sign-extend or truncate). Audit
///     Min-3: Fortran integers are always signed, so the int_extend
///     `signed` flag is hardcoded to `true`.
///   * Int ↔ Float (round to nearest for Float→Int).
///   * F32 ↔ F64 (extend / truncate).
///   * Bool ↔ Int (round-trip via int_extend; Fortran logicals
///     occupy a full kind so this is rare but legal).
///
/// Anything that doesn't match one of those cases falls into the
/// `_ => val` arm and a `debug_assert!` fires — silently passing
/// the wrong-typed value would let a future caller wire mismatched
/// types into a Store, which the verifier (after MAJOR-4) would
/// then catch much later. Better to fail loudly at the source.
pub(super) fn coerce_to_type(b: &mut FuncBuilder, val: ValueId, target: &IrType) -> ValueId {
    let src = match b.func().value_type(val) {
        Some(t) => t,
        None => return val,
    };
    if src == *target {
        return val;
    }
    match (&src, target) {
        // Complex values commonly travel as ptr<[f32/f64 x 2]> buffers.
        // When a by-value complex slot expects the aggregate itself,
        // materialize it by loading the pointed-to pair.
        (IrType::Ptr(inner), target)
            if matches!(inner.as_ref(), IrType::Array(_, 2)) && inner.as_ref() == target =>
        {
            b.load_typed(val, target.clone())
        }
        // Complex → real coercion (F2018 §10.2.1.3): assigning a
        // complex value to a real variable extracts its real
        // component.  Common in BLAS code like `rtemp = rtemp +
        // conjg(z)*z` where the RHS is mathematically real but
        // typed complex.
        (IrType::Ptr(inner), IrType::Float(target_fw))
            if matches!(inner.as_ref(),
                IrType::Array(elem, 2) if matches!(elem.as_ref(), IrType::Float(_))) =>
        {
            let elem_fw = match inner.as_ref() {
                IrType::Array(elem, _) => match elem.as_ref() {
                    IrType::Float(fw) => *fw,
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            };
            let zero = b.const_i64(0);
            let re_ptr = b.gep(val, vec![zero], IrType::Int(IntWidth::I8));
            let re = b.load_typed(re_ptr, IrType::Float(elem_fw));
            // Width adjust if target precision differs from element.
            if elem_fw == *target_fw {
                re
            } else if elem_fw == FloatWidth::F32 && *target_fw == FloatWidth::F64 {
                b.float_extend(re, FloatWidth::F64)
            } else {
                b.float_trunc(re, *target_fw)
            }
        }
        // Int → Complex (F2018 §10.1.10.1, F2018 §16.9.43): cmplx(i)
        // semantics — integer becomes the real part, imaginary part 0.
        // Common via `zero_value_for_ir_type` for missing optional
        // complex args returning a scalar zero, or coerce_value_call_arg
        // when an integer literal flows into a complex VALUE parameter.
        (IrType::Int(_), IrType::Array(elem, 2)) if matches!(elem.as_ref(), IrType::Float(_)) => {
            let fw = match elem.as_ref() {
                IrType::Float(fw) => *fw,
                _ => unreachable!(),
            };
            let re = b.int_to_float(val, fw);
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
            b.load_typed(buf, IrType::Array(Box::new(IrType::Float(fw)), 2))
        }
        // Real → Complex (F2018 §10.1.10.1): real becomes real part,
        // imaginary part 0. Width-adjusts when source and target
        // float widths differ (e.g. real(sp) literal → complex(dp)).
        (IrType::Float(src_fw), IrType::Array(elem, 2))
            if matches!(elem.as_ref(), IrType::Float(_)) =>
        {
            let target_fw = match elem.as_ref() {
                IrType::Float(fw) => *fw,
                _ => unreachable!(),
            };
            let re = if *src_fw == target_fw {
                val
            } else if *src_fw == FloatWidth::F32 && target_fw == FloatWidth::F64 {
                b.float_extend(val, FloatWidth::F64)
            } else {
                b.float_trunc(val, target_fw)
            };
            let zero = match target_fw {
                FloatWidth::F32 => b.const_f32(0.0),
                FloatWidth::F64 => b.const_f64(0.0),
            };
            let buf = b.alloca(IrType::Array(Box::new(IrType::Float(target_fw)), 2));
            let zero_off = b.const_i64(0);
            let lane_bytes = b.const_i64(if target_fw == FloatWidth::F64 { 8 } else { 4 });
            let re_ptr = b.gep(buf, vec![zero_off], IrType::Int(IntWidth::I8));
            let im_ptr = b.gep(buf, vec![lane_bytes], IrType::Int(IntWidth::I8));
            b.store(re, re_ptr);
            b.store(zero, im_ptr);
            b.load_typed(buf, IrType::Array(Box::new(IrType::Float(target_fw)), 2))
        }
        // Int → Float
        (IrType::Int(_), IrType::Float(fw)) => b.int_to_float(val, *fw),
        // Float → Int
        (IrType::Float(_), IrType::Int(iw)) => b.float_to_int(val, *iw),
        // F32 ↔ F64
        (IrType::Float(FloatWidth::F32), IrType::Float(FloatWidth::F64)) => {
            b.float_extend(val, FloatWidth::F64)
        }
        (IrType::Float(FloatWidth::F64), IrType::Float(FloatWidth::F32)) => {
            b.float_trunc(val, FloatWidth::F32)
        }
        // Int width change. Audit Min-3: Fortran integers are signed.
        (IrType::Int(src_w), IrType::Int(dst_w)) => {
            if dst_w.bits() > src_w.bits() {
                b.int_extend(val, *dst_w, true)
            } else if dst_w.bits() < src_w.bits() {
                b.int_trunc(val, *dst_w)
            } else {
                val
            }
        }
        // Bool ↔ Int via int_extend. Bool is i1 in our model.
        (IrType::Bool, IrType::Int(iw)) => b.int_extend(val, *iw, false),
        // Int → Bool: compare against zero to produce a true Bool
        // rather than truncating to i8 (which the verifier would
        // then reject on any .and./.or. operand). Common path:
        // LOGICAL fields in derived types load as i8 and need to
        // reach Bool before a logical op (audit31 Finding 13).
        (IrType::Int(_), IrType::Bool) => {
            let zero = match &src {
                IrType::Int(IntWidth::I64) => b.const_i64(0),
                IrType::Int(IntWidth::I16) => b.const_i32(0),
                IrType::Int(IntWidth::I8) => b.const_i32(0),
                _ => b.const_i32(0),
            };
            // Widen to i32 first if the source is narrower so
            // icmp gets matching operand widths.
            let widened = match &src {
                IrType::Int(IntWidth::I8) | IrType::Int(IntWidth::I16) => {
                    b.int_extend(val, IntWidth::I32, false)
                }
                _ => val,
            };
            b.icmp(CmpOp::Ne, widened, zero)
        }
        // Ptr<Array<T, N>> → Ptr<T>: pointer to array used as pointer to element.
        // Common for character arrays (Ptr<[i8 x 20]> → Ptr<i8>).
        (IrType::Ptr(_), IrType::Ptr(_)) => {
            // Pointers are all the same size on ARM64 — pass through.
            val
        }
        // Int → Ptr: value used in pointer context (e.g., byte as char*).
        (IrType::Int(_), IrType::Ptr(_)) => b.int_to_ptr(val, IrType::Int(IntWidth::I8)),
        // Ptr → Int: pointer used in integer context.
        (IrType::Ptr(_), IrType::Int(IntWidth::I64)) => b.ptr_to_int(val),
        (IrType::Ptr(_), IrType::Int(iw)) => {
            let i64_val = b.ptr_to_int(val);
            b.int_trunc(i64_val, *iw)
        }
        // Ptr<derived/byte-aggregate> → Float: nothing to do at the
        // IR level. This arises when generic dispatch picks a
        // wrong-typed specific (e.g. a structure constructor for a
        // type that has a same-named generic interface). Returning
        // the val unchanged would propagate a struct ptr to a store
        // that expects a float and trip the IR verifier; emit a
        // typed zero of the target so the call/store stays well-typed
        // (the call's runtime semantics are already broken — this
        // just keeps the verifier from rejecting the surrounding IR).
        (IrType::Ptr(_), IrType::Float(fw)) => match fw {
            FloatWidth::F32 => b.const_f32(0.0),
            FloatWidth::F64 => b.const_f64(0.0),
        },
        // Ptr<Bool> → Bool: dereference the pointer. Stdlib's masked
        // reductions (`stdlib_sum_1d_sp_mask` etc.) hit this when the
        // mask actual is passed by reference but the inner expression
        // expects a bool value. A typed load keeps the IR consistent
        // and the runtime correct.
        (IrType::Ptr(inner), IrType::Bool) if matches!(**inner, IrType::Bool) => {
            b.load_typed(val, IrType::Bool)
        }
        // Ptr<i8> → Bool: load the byte and treat it as a logical.
        // Fortran logical(1) flows through the IR as a pointer to i8
        // (the in-memory representation), but element-wise intrinsic
        // bodies (merge/where) expect the element value as a Bool.
        // Without this, merge() with a logical(1) mask array failed
        // to compile and crashed the IR verifier.
        (IrType::Ptr(inner), IrType::Bool) if matches!(**inner, IrType::Int(IntWidth::I8)) => {
            let byte = b.load_typed(val, IrType::Int(IntWidth::I8));
            let zero = b.const_int(0, IntWidth::I8);
            b.icmp(CmpOp::Ne, byte, zero)
        }
        _ => {
            eprintln!(
                "coerce_to_type: unhandled coercion {:?} → {:?}",
                src, target
            );
            val
        }
    }
}

pub(super) fn widen_to_i64(b: &mut FuncBuilder, value: ValueId) -> ValueId {
    match b.func().value_type(value) {
        Some(IrType::Int(IntWidth::I64)) => value,
        _ => coerce_to_type(b, value, &IrType::Int(IntWidth::I64)),
    }
}

pub(super) fn clamp_nonnegative_i64(b: &mut FuncBuilder, value: ValueId) -> ValueId {
    let widened = widen_to_i64(b, value);
    let zero = b.const_i64(0);
    let is_nonnegative = b.icmp(CmpOp::Ge, widened, zero);
    b.select(is_nonnegative, widened, zero)
}

pub(super) fn const_range_for_ir_type(ty: &IrType) -> Option<i128> {
    Some(match ty {
        IrType::Int(IntWidth::I8) => 2,
        IrType::Int(IntWidth::I16) => 4,
        IrType::Int(IntWidth::I32) => 9,
        IrType::Int(IntWidth::I64) => 18,
        IrType::Int(IntWidth::I128) => 38,
        IrType::Float(FloatWidth::F32) => 37,
        IrType::Float(FloatWidth::F64) => 307,
        _ => return None,
    })
}

/// Storage size in bits (F2018 §16.9.196 STORAGE_SIZE) for a value of
/// the given IR type. Walks pointer/array wrappers so callers passing
/// the *address* of a value still get the value's storage size.
pub(super) fn storage_size_bits_for_ir_type(ty: &IrType) -> i32 {
    match ty {
        IrType::Int(IntWidth::I8) => 8,
        IrType::Int(IntWidth::I16) => 16,
        IrType::Int(IntWidth::I32) => 32,
        IrType::Int(IntWidth::I64) => 64,
        IrType::Int(IntWidth::I128) => 128,
        IrType::Float(FloatWidth::F32) => 32,
        IrType::Float(FloatWidth::F64) => 64,
        IrType::Bool => 32,
        IrType::Array(elem, n) => {
            let elem_bits = storage_size_bits_for_ir_type(elem);
            elem_bits.saturating_mul(*n as i32)
        }
        // Pointer to a value — return the storage size of the pointee
        // (this is the form taken by descriptor-passed actuals).
        IrType::Ptr(inner) => storage_size_bits_for_ir_type(inner),
        _ => 0,
    }
}
