//! Target-neutral codegen identifiers (x03). Both backends use these;
//! everything else — instructions, operands, registers, functions —
//! stays per-backend until x05 shows what the allocator actually
//! shares.

/// Virtual register identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VRegId(pub u32);

/// Machine basic-block identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MBlockId(pub u32);

/// How many call sites lie strictly inside a live interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallCrossing {
    None,
    One(u32),
    Multiple,
}

/// Classify calls in the open interval `(start, end)` without materializing
/// a per-interval crossing vector. `sorted_calls` must be ascending.
pub fn classify_call_crossing(sorted_calls: &[u32], start: u32, end: u32) -> CallCrossing {
    let first = sorted_calls.partition_point(|&position| position <= start);
    let after_last = sorted_calls.partition_point(|&position| position < end);
    match after_last.saturating_sub(first) {
        0 => CallCrossing::None,
        1 => CallCrossing::One(sorted_calls[first]),
        _ => CallCrossing::Multiple,
    }
}

use crate::ir::types::{FloatWidth, IrType};
use std::fmt::Write;

#[cfg(test)]
mod call_crossing_tests {
    use super::*;

    #[test]
    fn classifies_open_interval_call_crossings() {
        let calls = [2, 4, 6, 8, 10];
        assert_eq!(classify_call_crossing(&calls, 4, 6), CallCrossing::None);
        assert_eq!(classify_call_crossing(&calls, 3, 5), CallCrossing::One(4));
        assert_eq!(classify_call_crossing(&calls, 1, 9), CallCrossing::Multiple);
        assert_eq!(classify_call_crossing(&[], 0, 10), CallCrossing::None);
    }
}

fn split_i128_words(value: i128) -> (u64, u64) {
    let bits = value as u128;
    (bits as u64, (bits >> 64) as u64)
}

fn emit_i128_words(out: &mut String, value: i128) {
    let (lo, hi) = split_i128_words(value);
    writeln!(out, "    .quad 0x{:016x}", lo).unwrap();
    writeln!(out, "    .quad 0x{:016x}", hi).unwrap();
}

fn emit_byte_values(out: &mut String, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let joined = bytes
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "    .byte {}", joined).unwrap();
}

fn byte_array_align_log2(byte_count: u64) -> u8 {
    if byte_count >= 8 {
        3
    } else if byte_count >= 4 {
        2
    } else if byte_count >= 2 {
        1
    } else {
        0
    }
}

fn float_bits_literal(ty: &IrType, value: f64) -> String {
    match ty {
        IrType::Float(FloatWidth::F32) => format!("0x{:08x}", (value as f32).to_bits()),
        IrType::Float(FloatWidth::F64) => format!("0x{:016x}", value.to_bits()),
        _ => value.to_string(),
    }
}

/// Object-format dialect for global emission. The data directives are
/// gas-portable; what differs is the section header, symbol naming
/// (Mach-O prefixes `_`), and linkage/metadata directives.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GlobalsDialect {
    MachO,
    Elf,
}

fn zero_global_align_bytes(ty: &IrType, layout: &crate::target::TargetLayout) -> u64 {
    ty.size_bytes(layout).clamp(1, 16).next_power_of_two()
}

fn emit_data_section(out: &mut String, dialect: GlobalsDialect, in_data: &mut bool) {
    if *in_data {
        return;
    }
    match dialect {
        GlobalsDialect::MachO => writeln!(out, ".section __DATA,__data").unwrap(),
        GlobalsDialect::Elf => writeln!(out, ".data").unwrap(),
    }
    *in_data = true;
}

fn emit_zero_fill_global(
    out: &mut String,
    symbol: &str,
    ty: &IrType,
    layout: &crate::target::TargetLayout,
    dialect: GlobalsDialect,
    is_module_global: bool,
) {
    let size = ty.size_bytes(layout).max(1);
    let align = zero_global_align_bytes(ty, layout);
    match dialect {
        GlobalsDialect::Elf => {
            if !is_module_global {
                writeln!(out, ".local {}", symbol).unwrap();
            }
            writeln!(out, ".comm {},{},{}", symbol, size, align).unwrap();
        }
        GlobalsDialect::MachO => {
            if is_module_global {
                writeln!(out, ".globl {}", symbol).unwrap();
            } else {
                writeln!(out, ".private_extern {}", symbol).unwrap();
            }
            writeln!(
                out,
                ".zerofill __DATA,__bss,{},{},{}",
                symbol,
                size,
                align.trailing_zeros()
            )
            .unwrap();
        }
    }
}

/// Emit module-level globals as data or zero-fill reservations.
/// Each global gets a label and a directive matching its type
/// (`.long`, `.quad`, `.single`, `.double`, etc.) plus the
/// initializer value. Zero-initialized globals reserve NOBITS storage
/// instead of materializing bytes in the output file.
///
/// Array-typed globals: the IR type is `Array<i8, byte_size>` so
/// the element count isn't directly recoverable from the type.
/// The caller must use `IntArray`/`FloatArray` initializers that
/// carry the element count explicitly.
///
/// Module globals (`afs_mod_*` and `afs_common_*`) are emitted as
/// `.globl` so other translation units can reference them via USE.
/// Non-module globals (SAVE-promoted locals) stay `.private_extern`
/// to prevent cross-TU collisions (audit Maj-1).
pub fn emit_globals(
    globals: &[crate::ir::inst::Global],
    layout: &crate::target::TargetLayout,
    dialect: GlobalsDialect,
) -> String {
    use crate::ir::inst::GlobalInit;
    use crate::ir::types::{FloatWidth, IntWidth, IrType};

    let mut out = String::new();
    if globals.is_empty() {
        return out;
    }

    let mut in_data = false;
    for g in globals {
        let symbol = match dialect {
            GlobalsDialect::MachO if !g.name.starts_with('_') => format!("_{}", g.name),
            _ => g.name.clone(),
        };
        // Module globals need external linkage for multi-file. Type-bound-
        // procedure vtables (`afs_vtable_*`) are owned by one TU but
        // referenced by consumers dispatching through a type they never
        // saw the source of, so they need external linkage too.
        let is_module_global = g.name.starts_with("afs_mod_")
            || g.name.starts_with("afs_common_")
            || g.name.starts_with("afs_vtable_");

        // Uninitialized COMMON storage is a COMMON symbol (.comm) on
        // BOTH formats: every TU declaring /blk/ emits the same
        // symbols and the linker merges them. A strong per-TU
        // definition is a duplicate-symbol error (found cross-TU by
        // x08's differential — on ELF first, then Mach-O in CI).
        // Initialized commons (BLOCK DATA) stay strong: one TU owns
        // the initializer.
        let zero_init = matches!(
            g.initializer,
            None | Some(crate::ir::inst::GlobalInit::Zero)
        );
        if g.name.starts_with("afs_common_") && zero_init {
            let size = g.ty.size_bytes(layout).max(1);
            let pow2 = zero_global_align_bytes(&g.ty, layout);
            match dialect {
                GlobalsDialect::Elf => {
                    writeln!(out, ".comm {},{},{}", symbol, size, pow2).unwrap();
                }
                GlobalsDialect::MachO => {
                    // Mach-O .comm takes log2 alignment.
                    writeln!(out, ".comm {},{},{}", symbol, size, pow2.trailing_zeros()).unwrap();
                }
            }
            continue;
        }
        if zero_init {
            emit_zero_fill_global(&mut out, &symbol, &g.ty, layout, dialect, is_module_global);
            continue;
        }

        emit_data_section(&mut out, dialect, &mut in_data);

        match (dialect, is_module_global) {
            (GlobalsDialect::MachO, true) => writeln!(out, ".globl {}", symbol).unwrap(),
            (GlobalsDialect::MachO, false) => writeln!(out, ".private_extern {}", symbol).unwrap(),
            (GlobalsDialect::Elf, vis) => {
                if vis {
                    writeln!(out, ".globl {}", symbol).unwrap();
                } else {
                    writeln!(out, ".local {}", symbol).unwrap();
                }
                writeln!(out, ".type {}, @object", symbol).unwrap();
                writeln!(out, ".size {}, {}", symbol, g.ty.size_bytes(layout)).unwrap();
            }
        }

        // Vtables and other quad tables carry a mix of raw integers and
        // symbol-address relocations. Emit one `.quad` per slot; symbol
        // slots get the platform symbol prefix (same rule as the table's
        // own name) so `.quad symbol` becomes a data relocation the
        // assembler resolves (ARM64_RELOC_UNSIGNED / R_X86_64_64).
        if let Some(GlobalInit::QuadTable(slots)) = &g.initializer {
            use crate::ir::inst::QuadSlot;
            writeln!(out, ".p2align 3").unwrap();
            writeln!(out, "{}:", symbol).unwrap();
            for slot in slots {
                match slot {
                    QuadSlot::Int(v) => writeln!(out, "    .quad {}", v).unwrap(),
                    QuadSlot::Sym(name) => {
                        let sym = match dialect {
                            GlobalsDialect::MachO if !name.starts_with('_') => {
                                format!("_{}", name)
                            }
                            _ => name.clone(),
                        };
                        writeln!(out, "    .quad {}", sym).unwrap();
                    }
                }
            }
            continue;
        }

        // Array globals carry `Array<elem_ty, count>`.  Pick the
        // directive from the element type. Float arrays are emitted
        // as exact bit patterns; large finite decimal literals can
        // exceed what Apple's assembler accepts for `.single`.
        if let IrType::Array(elem_ty, count) = &g.ty {
            let (align, directive, _elem_bytes, is_float, float_lane_ty) = match elem_ty.as_ref() {
                IrType::Int(IntWidth::I8) | IrType::Bool => {
                    (byte_array_align_log2(*count), ".byte", 1, false, None)
                }
                IrType::Int(IntWidth::I16) => (1, ".short", 2, false, None),
                IrType::Int(IntWidth::I32) => (2, ".long", 4, false, None),
                IrType::Int(IntWidth::I64) => (3, ".quad", 8, false, None),
                IrType::Int(IntWidth::I128) => (4, ".quad", 16, false, None),
                IrType::Float(FloatWidth::F32) => (2, ".long", 4, true, Some(elem_ty.as_ref())),
                IrType::Float(FloatWidth::F64) => (3, ".quad", 8, true, Some(elem_ty.as_ref())),
                IrType::Array(inner, _)
                    if matches!(inner.as_ref(), IrType::Float(FloatWidth::F32)) =>
                {
                    (2, ".long", 4, true, Some(inner.as_ref()))
                }
                IrType::Array(inner, _)
                    if matches!(inner.as_ref(), IrType::Float(FloatWidth::F64)) =>
                {
                    (3, ".quad", 8, true, Some(inner.as_ref()))
                }
                _ => (3, ".quad", 8, false, None),
            };
            if align > 0 {
                writeln!(out, ".p2align {}", align).unwrap();
            }
            writeln!(out, "{}:", symbol).unwrap();
            match &g.initializer {
                Some(GlobalInit::IntArray(vs))
                    if matches!(elem_ty.as_ref(), IrType::Int(IntWidth::I128)) =>
                {
                    for v in vs {
                        emit_i128_words(&mut out, *v);
                    }
                }
                Some(GlobalInit::IntArray(vs)) if !is_float => {
                    for v in vs {
                        writeln!(out, "    {} {}", directive, v).unwrap();
                    }
                }
                Some(GlobalInit::FloatArray(vs)) if is_float => {
                    let float_lane_ty = float_lane_ty.unwrap_or(elem_ty.as_ref());
                    for v in vs {
                        writeln!(
                            out,
                            "    {} {}",
                            directive,
                            float_bits_literal(float_lane_ty, *v)
                        )
                        .unwrap();
                    }
                }
                Some(GlobalInit::String(bytes)) => {
                    emit_byte_values(&mut out, bytes);
                    let total_bytes = g.ty.size_bytes(layout) as usize;
                    if bytes.len() < total_bytes {
                        writeln!(out, "    .space {}", total_bytes - bytes.len()).unwrap();
                    }
                }
                _ => {
                    // Nested arrays (for example arrays of byte-packed derived
                    // values) don't have a scalar element directive. Emit their
                    // zero-initialized storage using the full IR type size
                    // instead of falling back to a bogus ".quad * count" size.
                    let byte_size = g.ty.size_bytes(layout);
                    writeln!(out, "    .space {}", byte_size).unwrap();
                }
            }
            continue;
        }

        if matches!(g.ty, IrType::Int(IntWidth::I128)) {
            writeln!(out, ".p2align 4").unwrap();
            writeln!(out, "{}:", symbol).unwrap();
            match &g.initializer {
                Some(GlobalInit::Int(v)) => emit_i128_words(&mut out, *v),
                Some(GlobalInit::Zero) | None => emit_i128_words(&mut out, 0),
                _ => writeln!(out, "    .space 16").unwrap(),
            }
            continue;
        }

        // Scalar globals: pick alignment + storage directive. Floats
        // always use bit-pattern emission, not decimal assembler
        // literals. This keeps NaN/Inf portable and avoids rejected
        // fixed-decimal forms for very large finite PARAMETER values.
        let (align, directive, default_zero) = match &g.ty {
            IrType::Int(IntWidth::I8) | IrType::Bool => (0, ".byte", "0"),
            IrType::Int(IntWidth::I16) => (1, ".short", "0"),
            IrType::Int(IntWidth::I32) => (2, ".long", "0"),
            IrType::Int(IntWidth::I64) => (3, ".quad", "0"),
            IrType::Float(FloatWidth::F32) => (2, ".long", "0"),
            IrType::Float(FloatWidth::F64) => (3, ".quad", "0"),
            _ => (3, ".quad", "0"), // pointers and aggregates: 8-byte slot
        };
        if align > 0 {
            writeln!(out, ".p2align {}", align).unwrap();
        }
        writeln!(out, "{}:", symbol).unwrap();
        let value = match &g.initializer {
            Some(GlobalInit::Int(v)) => v.to_string(),
            Some(GlobalInit::Float(v)) => float_bits_literal(&g.ty, *v),
            Some(GlobalInit::Zero) | None => default_zero.into(),
            Some(GlobalInit::String(bytes))
                if matches!(g.ty, IrType::Int(IntWidth::I8) | IrType::Bool) =>
            {
                bytes.first().copied().unwrap_or(0).to_string()
            }
            Some(GlobalInit::String(_)) => default_zero.into(),
            Some(GlobalInit::IntArray(_)) | Some(GlobalInit::FloatArray(_)) => {
                // An array initializer on a scalar-typed global is an IR
                // invariant violation: array-typed globals are emitted by the
                // array arm above, which `continue`s before this scalar path.
                // Emitting zero would silently drop the initializer data
                // (audit T3); fail loudly at the type/initializer mismatch.
                panic!(
                    "global '{symbol}': array initializer on scalar-typed \
                     global {:?} — IR type/initializer mismatch",
                    g.ty
                );
            }
            // QuadTable globals are emitted by the dedicated arm above
            // and never reach this scalar path.
            Some(GlobalInit::QuadTable(_)) => default_zero.into(),
        };
        writeln!(out, "    {} {}", directive, value).unwrap();
    }
    out
}
