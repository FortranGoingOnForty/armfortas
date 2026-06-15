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

use crate::ir::types::{FloatWidth, IrType};
use std::fmt::Write;

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

/// Emit module-level globals as a data section.
/// Each global gets a label and a directive matching its type
/// (`.long`, `.quad`, `.single`, `.double`, etc.) plus the
/// initializer value. Zero-initialized globals still emit an
/// explicit zero so the symbol resolves at link time.
///
/// Array-typed globals: the IR type is `Array<i8, byte_size>` so
/// the element count isn't directly recoverable from the type.
/// The caller must use `IntArray`/`FloatArray` initializers that
/// carry the element count explicitly. Zero-initialized arrays
/// fall back to `.space byte_size`.
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

    match dialect {
        GlobalsDialect::MachO => writeln!(out, ".section __DATA,__data").unwrap(),
        GlobalsDialect::Elf => writeln!(out, ".data").unwrap(),
    }
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
            let pow2 = size.min(16).next_power_of_two();
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
                // Array initializer on a scalar-typed global —
                // shouldn't happen, but emit zero as a safe fallback.
                default_zero.into()
            }
            // QuadTable globals are emitted by the dedicated arm above
            // and never reach this scalar path.
            Some(GlobalInit::QuadTable(_)) => default_zero.into(),
        };
        writeln!(out, "    {} {}", directive, value).unwrap();
    }
    out
}
