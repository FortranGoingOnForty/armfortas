//! AAPCS64 argument classification and stack layout.
//!
//! Apple's AArch64 procedure-call standard, as implemented for armfortas's
//! lowering layer. Covers integer (general-purpose) registers `x0-x7`,
//! floating-point `s0-s7` / `d0-d7`, NEON vector `v0-v7`, the i128 register
//! pair lane (`x{N}, x{N+1}`), and stack-overflow assignment with proper
//! per-type alignment.
//!
//! Extracted from `isel.rs` so the ABI assumption lives in one place. Any
//! future ABI variant (e.g. Linux AArch64, with its different va_list shape)
//! goes here, not in instruction selection.

use crate::ir::types::{FloatWidth, IntWidth, IrType};

/// Where a single argument lives after AAPCS64 classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiArgLoc {
    Gp(u8),
    Gp32(u8),
    Fp(u8),
    Fp32(u8),
    GpPair(u8),
    /// 128-bit NEON vector via `v0-v7`. Per AAPCS64, vectors share
    /// the same physical bank as floats; the V form is the 128-bit
    /// view of the same register.
    V128(u8),
    Stack(i64),
}

/// Running tally of consumed argument slots, used while classifying a
/// signature one parameter at a time.
#[derive(Debug, Default, Clone, Copy)]
pub struct AbiArgState {
    pub gp_idx: u8,
    pub fp_idx: u8,
    pub stack_offset: i64,
}

/// Round `value` up to the next multiple of `align` (which must be a
/// power of two).
pub fn align_to(value: i64, align: i64) -> i64 {
    debug_assert!(align > 0 && (align & (align - 1)) == 0);
    (value + align - 1) & !(align - 1)
}

/// `(size, align)` of a type when passed on the stack overflow area
/// per AAPCS64 §6.4. Mostly the natural ABI size; vectors are 16-byte
/// aligned, i128 takes a 16-byte slot.
pub fn abi_stack_layout(ty: &IrType) -> (i64, i64) {
    match ty {
        IrType::Int(IntWidth::I128) => (16, 16),
        IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_) => (8, 8),
        IrType::Float(FloatWidth::F64) => (8, 8),
        // TODO: integer(c_short), value actuals still appear widened before
        // reaching call lowering, so end-to-end 16-bit VALUE ABI parity needs
        // a follow-up beyond this stack-packing fix.
        IrType::Float(FloatWidth::F32) => (4, 4),
        IrType::Int(IntWidth::I32) => (4, 4),
        IrType::Int(IntWidth::I16) => (2, 2),
        IrType::Int(IntWidth::I8) | IrType::Bool => (1, 1),
        IrType::Vector { .. } => (16, 16),
        _ => (8, 8),
    }
}

/// Place one argument according to AAPCS64. Updates `state` in-place
/// to reflect the consumed slot.
pub fn classify_abi_arg(ty: &IrType, state: &mut AbiArgState) -> AbiArgLoc {
    match ty {
        IrType::Int(IntWidth::I128) => {
            if state.gp_idx + 2 <= 8 {
                let reg = state.gp_idx;
                state.gp_idx += 2;
                AbiArgLoc::GpPair(reg)
            } else {
                state.gp_idx = 8;
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        IrType::Float(FloatWidth::F64) => {
            if state.fp_idx < 8 {
                let reg = state.fp_idx;
                state.fp_idx += 1;
                AbiArgLoc::Fp(reg)
            } else {
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        IrType::Float(FloatWidth::F32) => {
            if state.fp_idx < 8 {
                let reg = state.fp_idx;
                state.fp_idx += 1;
                AbiArgLoc::Fp32(reg)
            } else {
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        IrType::Int(IntWidth::I8)
        | IrType::Int(IntWidth::I16)
        | IrType::Int(IntWidth::I32)
        | IrType::Bool => {
            if state.gp_idx < 8 {
                let reg = state.gp_idx;
                state.gp_idx += 1;
                AbiArgLoc::Gp32(reg)
            } else {
                state.gp_idx = 8;
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        IrType::Vector { .. } => {
            // AAPCS64 §6.4.2: vector args pass in v0-v7, sharing the
            // same idx counter as float args (the V registers ARE the
            // 128-bit form of the same physical regs).
            if state.fp_idx < 8 {
                let reg = state.fp_idx;
                state.fp_idx += 1;
                AbiArgLoc::V128(reg)
            } else {
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        _ => {
            if state.gp_idx < 8 {
                let reg = state.gp_idx;
                state.gp_idx += 1;
                AbiArgLoc::Gp(reg)
            } else {
                state.gp_idx = 8;
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
    }
}

#[cfg(test)]
mod layout_drift_tests {
    use super::*;
    use crate::target::TargetLayout;

    /// x02 drift guard: `abi_stack_layout` answers "stack slot when
    /// passed" and deliberately does NOT route through `TargetLayout`
    /// (x04's classifier owns call-ABI shape). The duplication is by
    /// design; silent drift on the values they share is not.
    #[test]
    fn abi_stack_layout_agrees_with_target_layout_on_shared_values() {
        let l = TargetLayout::LP64;
        assert_eq!(
            abi_stack_layout(&IrType::Int(IntWidth::I128)),
            (16, l.i128_align as i64)
        );
        assert_eq!(
            abi_stack_layout(&IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))),
            (l.ptr_bytes as i64, l.ptr_align as i64)
        );
        assert_eq!(abi_stack_layout(&IrType::Float(FloatWidth::F64)), (8, 8));
        assert_eq!(
            abi_stack_layout(&IrType::Vector {
                lanes: 4,
                elem: Box::new(IrType::Float(FloatWidth::F32))
            }),
            (l.vector_bytes as i64, l.vector_bytes as i64)
        );
    }

    /// The compiler-side array_descriptor() must reproduce the 384-byte
    /// contract that runtime/src/descriptor.rs asserts independently
    /// (`size_of::<ArrayDescriptor>() == 384`). If these disagree, the
    /// accessor formula is wrong, not the runtime constant.
    #[test]
    fn descriptor_accessors_match_runtime_contract() {
        let l = TargetLayout::LP64;
        assert_eq!(l.array_descriptor().0, 384, "see runtime/src/descriptor.rs");
        assert_eq!(
            l.string_descriptor().0,
            32,
            "see runtime/src/descriptor.rs StringDescriptor"
        );
    }
}
