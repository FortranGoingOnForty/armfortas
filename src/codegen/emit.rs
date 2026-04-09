//! Assembly text emission — converts Machine IR to ARM64 assembly text.
//!
//! Produces output compatible with both afs-as and Apple's system assembler.

use std::fmt::Write;
use super::mir::*;

/// Emit module-level globals as a `.section __DATA,__data` block.
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
/// Symbols are emitted as `.private_extern` (not `.globl`) per
/// audit Maj-1 so they can't collide across translation units.
pub fn emit_globals(globals: &[crate::ir::inst::Global]) -> String {
    use crate::ir::inst::GlobalInit;
    use crate::ir::types::{IrType, IntWidth, FloatWidth};

    let mut out = String::new();
    if globals.is_empty() {
        return out;
    }

    writeln!(out, ".section __DATA,__data").unwrap();
    for g in globals {
        let symbol = if g.name.starts_with('_') {
            g.name.clone()
        } else {
            format!("_{}", g.name)
        };
        writeln!(out, ".private_extern {}", symbol).unwrap();

        // Array globals carry `Array<elem_ty, count>`.  Pick the
        // directive from the element type so `.long` / `.quad` /
        // `.single` / `.double` all work correctly.
        if let IrType::Array(elem_ty, count) = &g.ty {
            let (align, directive, elem_bytes, is_float) = match elem_ty.as_ref() {
                IrType::Int(IntWidth::I8) | IrType::Bool => (0, ".byte",   1, false),
                IrType::Int(IntWidth::I16)               => (1, ".short",  2, false),
                IrType::Int(IntWidth::I32)               => (2, ".long",   4, false),
                IrType::Int(IntWidth::I64)               => (3, ".quad",   8, false),
                IrType::Float(FloatWidth::F32)           => (2, ".single", 4, true),
                IrType::Float(FloatWidth::F64)           => (3, ".double", 8, true),
                _ => (3, ".quad", 8, false),
            };
            if align > 0 {
                writeln!(out, ".p2align {}", align).unwrap();
            }
            writeln!(out, "{}:", symbol).unwrap();
            match &g.initializer {
                Some(GlobalInit::IntArray(vs)) if !is_float => {
                    for v in vs {
                        writeln!(out, "    {} {}", directive, v).unwrap();
                    }
                }
                Some(GlobalInit::FloatArray(vs)) if is_float => {
                    for v in vs {
                        writeln!(out, "    {} {}", directive, v).unwrap();
                    }
                }
                _ => {
                    let byte_size = count * (elem_bytes as u64);
                    writeln!(out, "    .space {}", byte_size).unwrap();
                }
            }
            continue;
        }

        // Scalar globals: pick alignment + storage directive.
        // Audit Med-5: NaN/Inf must round-trip portably across
        // assemblers. Apple's `as` accepts `.single NaN` but GNU
        // binutils does not. Emit non-finite floats as their
        // bit-pattern via `.long` / `.quad` so the same .s file
        // assembles cleanly on both.
        let is_nonfinite_float = matches!(
            (&g.ty, &g.initializer),
            (IrType::Float(_), Some(GlobalInit::Float(v))) if !v.is_finite()
        );
        let (align, directive, default_zero) = if is_nonfinite_float {
            match &g.ty {
                IrType::Float(FloatWidth::F32) => (2, ".long", "0"),
                _ => (3, ".quad", "0"),
            }
        } else {
            match &g.ty {
                IrType::Int(IntWidth::I8) | IrType::Bool => (0, ".byte",   "0"),
                IrType::Int(IntWidth::I16)               => (1, ".short",  "0"),
                IrType::Int(IntWidth::I32)               => (2, ".long",   "0"),
                IrType::Int(IntWidth::I64)               => (3, ".quad",   "0"),
                IrType::Float(FloatWidth::F32)           => (2, ".single", "0.0"),
                IrType::Float(FloatWidth::F64)           => (3, ".double", "0.0"),
                _ => (3, ".quad", "0"), // pointers and aggregates: 8-byte slot
            }
        };
        if align > 0 {
            writeln!(out, ".p2align {}", align).unwrap();
        }
        writeln!(out, "{}:", symbol).unwrap();
        let value = match &g.initializer {
            Some(GlobalInit::Int(v))    => v.to_string(),
            Some(GlobalInit::Float(v))  => {
                if v.is_finite() {
                    format!("{}", v)
                } else {
                    // Bit-pattern emission for NaN / ±Inf.
                    match &g.ty {
                        IrType::Float(FloatWidth::F32) => {
                            format!("0x{:08x}", (*v as f32).to_bits())
                        }
                        _ => format!("0x{:016x}", v.to_bits()),
                    }
                }
            }
            Some(GlobalInit::Zero) | None => default_zero.into(),
            Some(GlobalInit::String(_)) => default_zero.into(), // strings TBD
            Some(GlobalInit::IntArray(_)) | Some(GlobalInit::FloatArray(_)) => {
                // Array initializer on a scalar-typed global —
                // shouldn't happen, but emit zero as a safe fallback.
                default_zero.into()
            }
        };
        writeln!(out, "    {} {}", directive, value).unwrap();
    }
    out
}

/// Emit a machine function as ARM64 assembly text.
pub fn emit_function(mf: &MachineFunction) -> String {
    let mut out = String::new();

    // Function directive.
    writeln!(out, ".globl _{}", mf.name).unwrap();
    writeln!(out, ".p2align 2").unwrap();
    writeln!(out, "_{}:", mf.name).unwrap();

    for block in &mf.blocks {
        // Don't re-emit entry label (it's the function label).
        if block.id != MBlockId(0) {
            writeln!(out, "{}:", block.label).unwrap();
        }

        for inst in &block.insts {
            writeln!(out, "    {}", emit_inst(inst, mf)).unwrap();
        }
    }

    // Constant pool.
    if !mf.const_pool.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, ".section __DATA,__const").unwrap();
        for (i, entry) in mf.const_pool.iter().enumerate() {
            let label = const_pool_label(&mf.name, i as u32);
            match entry {
                ConstPoolEntry::F32(v) => {
                    writeln!(out, ".p2align 2").unwrap();
                    writeln!(out, "{}:", label).unwrap();
                    writeln!(out, "    .single {}", v).unwrap();
                }
                ConstPoolEntry::F64(v) => {
                    writeln!(out, ".p2align 3").unwrap();
                    writeln!(out, "{}:", label).unwrap();
                    writeln!(out, "    .double {}", v).unwrap();
                }
                ConstPoolEntry::I64(v) => {
                    writeln!(out, ".p2align 3").unwrap();
                    writeln!(out, "{}:", label).unwrap();
                    writeln!(out, "    .quad {}", v).unwrap();
                }
                ConstPoolEntry::Bytes(b) => {
                    writeln!(out, ".p2align 3").unwrap();
                    writeln!(out, "{}:", label).unwrap();
                    write!(out, "    .ascii \"").unwrap();
                    for &byte in b {
                        match byte {
                            b'\\' => write!(out, "\\\\").unwrap(),
                            b'"' => write!(out, "\\\"").unwrap(),
                            b'\n' => write!(out, "\\n").unwrap(),
                            b'\t' => write!(out, "\\t").unwrap(),
                            b if b.is_ascii_graphic() || b == b' ' => {
                                write!(out, "{}", b as char).unwrap();
                            }
                            b => write!(out, "\\x{:02x}", b).unwrap(),
                        }
                    }
                    writeln!(out, "\"").unwrap();
                }
            }
        }
    }

    out
}

/// Format `OP sp, sp, #N` (or `add x29, sp, #N`), falling back
/// to a 2-3 instruction synthesized sequence via the AAPCS64
/// scratch register x16 (IP0) when N exceeds the 12-bit
/// immediate range. x16 is free in the prologue/epilogue per
/// AAPCS64 — it has no caller-saved value at function entry
/// and can be clobbered before/after the FP/LR save.
///
/// Audit6 BLOCKING-5 (related to BLOCKING-4): functions whose
/// frame size exceeds 4095 bytes used to emit raw
/// `sub sp, sp, #4144` and the assembler rejected the
/// immediate. This came up after audit6 BLOCKING-4 added
/// per-allocate descriptor buffers, but it's a latent bug that
/// any large-frame function would hit.
fn fmt_sp_imm(op: &str, dest: &str, base: &str, n: i64) -> String {
    if (0..=4095).contains(&n) {
        return format!("{} {}, {}, #{}", op, dest, base, n);
    }
    // Synthesize the immediate in x16 then use the register form.
    let lo = n & 0xFFFF;
    let hi = (n >> 16) & 0xFFFF;
    let mov = if hi == 0 {
        format!("movz x16, #{}", lo)
    } else {
        format!("movz x16, #{}\n    movk x16, #{}, lsl #16", lo, hi)
    };
    format!("{}\n    {} {}, {}, x16", mov, op, dest, base)
}

/// Emit a single machine instruction as assembly text.
fn emit_inst(inst: &MachineInst, mf: &MachineFunction) -> String {
    match inst.opcode {
        ArmOpcode::AddReg => format!("add {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::AddImm => {
            let dest = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            let imm: i64 = match &inst.operands[2] {
                MachineOperand::FrameSlot(off) => *off as i64,
                MachineOperand::Imm(-1) => {
                    // Sentinel: prologue FP setup → frame_size - 16
                    mf.frame.size.saturating_sub(16) as i64
                }
                MachineOperand::Imm(v) => *v,
                _ => return format!("add {}, {}, {}",
                    dest, base, op_str(&inst.operands[2])),
            };
            // Both `add x29, sp, #N` (FP setup) and `add Xd, Xn, #N`
            // need the > 4095 fallback. Use the same scratch
            // synthesis since x16 is safe in the prologue.
            fmt_sp_imm("add", &dest, &base, imm)
        }
        ArmOpcode::SubReg => format!("sub {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::SubImm => {
            let imm: i64 = match &inst.operands[2] {
                MachineOperand::Imm(-1) => {
                    // Sentinel: epilogue SP restore → frame_size - 16
                    mf.frame.size.saturating_sub(16) as i64
                }
                MachineOperand::Imm(v) => *v,
                _ => 0,
            };
            let dest = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            fmt_sp_imm("sub", &dest, &base, imm)
        }
        ArmOpcode::Mul => format!("mul {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::Sdiv => format!("sdiv {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::Msub => format!("msub {}, {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]),
            op_str(&inst.operands[2]), op_str(&inst.operands[3])),
        ArmOpcode::Neg => format!("neg {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),

        ArmOpcode::AndReg => format!("and {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::OrrReg => format!("orr {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::EorReg => format!("eor {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::OrnReg => format!("orn {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::LslReg => format!("lsl {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::LsrReg => format!("lsr {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::AsrReg => format!("asr {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),

        ArmOpcode::Mvn => format!("mvn {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::Clz => format!("clz {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::Rbit => format!("rbit {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),

        ArmOpcode::CmpReg => format!("cmp {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::CmpImm => format!("cmp {}, #{}",
            op_str(&inst.operands[0]),
            if let MachineOperand::Imm(v) = &inst.operands[1] { *v } else { 0 }),
        ArmOpcode::Cset | ArmOpcode::FCset => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[1] { cond_str(*c) } else { "eq" };
            format!("cset {}, {}", op_str(&inst.operands[0]), cond)
        }
        ArmOpcode::CselReg => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[3] { cond_str(*c) } else { "eq" };
            format!("csel {}, {}, {}, {}", op_str(&inst.operands[0]),
                op_str(&inst.operands[1]), op_str(&inst.operands[2]), cond)
        }
        ArmOpcode::FCmpReg => format!("fcmp {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::FcselReg => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[3] { cond_str(*c) } else { "eq" };
            format!("fcsel {}, {}, {}, {}", op_str(&inst.operands[0]),
                op_str(&inst.operands[1]), op_str(&inst.operands[2]), cond)
        }

        ArmOpcode::FaddS | ArmOpcode::FaddD => format!("fadd {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::FsubS | ArmOpcode::FsubD => format!("fsub {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::FmulS | ArmOpcode::FmulD => format!("fmul {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::FdivS | ArmOpcode::FdivD => format!("fdiv {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::FnegS | ArmOpcode::FnegD => format!("fneg {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::FabsS | ArmOpcode::FabsD => format!("fabs {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::FsqrtS | ArmOpcode::FsqrtD => format!("fsqrt {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        // Fused multiply-add/subtract: 4-operand (dest, Sn, Sm, Sa).
        // FMADD  Sd, Sn, Sm, Sa → Sd = Sa + Sn*Sm
        // FMSUB  Sd, Sn, Sm, Sa → Sd = Sa - Sn*Sm
        // FNMSUB Sd, Sn, Sm, Sa → Sd = Sn*Sm - Sa
        ArmOpcode::FmaddS | ArmOpcode::FmaddD => format!("fmadd {}, {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]),
            op_str(&inst.operands[2]), op_str(&inst.operands[3])),
        ArmOpcode::FmsubS | ArmOpcode::FmsubD => format!("fmsub {}, {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]),
            op_str(&inst.operands[2]), op_str(&inst.operands[3])),
        ArmOpcode::FnmsubS | ArmOpcode::FnmsubD => format!("fnmsub {}, {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]),
            op_str(&inst.operands[2]), op_str(&inst.operands[3])),

        ArmOpcode::ScvtfSW | ArmOpcode::ScvtfDW |
        ArmOpcode::ScvtfSX | ArmOpcode::ScvtfDX => format!("scvtf {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::FcvtzsWS | ArmOpcode::FcvtzsWD |
        ArmOpcode::FcvtzsXS | ArmOpcode::FcvtzsXD => format!("fcvtzs {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::FcvtSD | ArmOpcode::FcvtDS => format!("fcvt {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),

        ArmOpcode::Movz => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[1] { *v } else { 0 };
            let shift = if let MachineOperand::Shift(s) = &inst.operands[2] { *s } else { 0 };
            if shift == 0 {
                format!("movz {}, #{}", op_str(&inst.operands[0]), imm)
            } else {
                format!("movz {}, #{}, lsl #{}", op_str(&inst.operands[0]), imm, shift)
            }
        }
        ArmOpcode::Movk => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[1] { *v } else { 0 };
            let shift = if let MachineOperand::Shift(s) = &inst.operands[2] { *s } else { 0 };
            format!("movk {}, #{}, lsl #{}", op_str(&inst.operands[0]), imm, shift)
        }
        ArmOpcode::Movn => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[1] { *v } else { 0 };
            let shift = if let MachineOperand::Shift(s) = &inst.operands[2] { *s } else { 0 };
            format!("movn {}, #{}, lsl #{}", op_str(&inst.operands[0]), imm, shift)
        }
        ArmOpcode::MovReg => {
            let dest = op_str(&inst.operands[0]);
            let src = op_str(&inst.operands[1]);
            // Handle width mismatch: w→x extend or x→w truncate.
            let dest_is_x = dest.starts_with('x');
            let src_is_w = src.starts_with('w');
            if dest_is_x && src_is_w {
                // Zero-extend 32→64: use uxtw.
                format!("uxtw {}, {}", dest, src)
            } else {
                format!("mov {}, {}", dest, src)
            }
        }
        ArmOpcode::FmovReg => format!("fmov {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),

        ArmOpcode::LdrImm | ArmOpcode::LdrFpImm
        | ArmOpcode::LdrsbImm | ArmOpcode::LdrshImm => {
            let dest = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            let offset_val = match &inst.operands[2] {
                MachineOperand::FrameSlot(off) => *off as i64,
                MachineOperand::Imm(v) => *v,
                _ => 0,
            };
            // Pick the mnemonic by opcode. LDRSB / LDRSH expect a
            // Wt destination (sign-extended into the lower 32 bits);
            // the dest operand is already a Gp32 vreg in those
            // cases, so the formatted register name is `w_`.
            let mnemonic = match inst.opcode {
                ArmOpcode::LdrsbImm => "ldrsb",
                ArmOpcode::LdrshImm => "ldrsh",
                _ => "ldr",
            };
            if (-256..=255).contains(&offset_val) {
                format!("{} {}, [{}, #{}]", mnemonic, dest, base, offset_val)
            } else {
                // Large offset: compute address in x8, then load.
                format!("mov x8, #{}\n    add x8, {}, x8\n    {} {}, [x8]",
                    offset_val, base, mnemonic, dest)
            }
        }
        ArmOpcode::StrImm | ArmOpcode::StrFpImm
        | ArmOpcode::StrbImm | ArmOpcode::StrhImm => {
            let src = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            let offset_val = match &inst.operands[2] {
                MachineOperand::FrameSlot(off) => *off as i64,
                MachineOperand::Imm(v) => *v,
                _ => 0,
            };
            let mnemonic = match inst.opcode {
                ArmOpcode::StrbImm => "strb",
                ArmOpcode::StrhImm => "strh",
                _ => "str",
            };
            if (-256..=255).contains(&offset_val) {
                format!("{} {}, [{}, #{}]", mnemonic, src, base, offset_val)
            } else {
                // Large offset: compute address in x8, then store.
                format!("mov x8, #{}\n    add x8, {}, x8\n    {} {}, [x8]",
                    offset_val, base, mnemonic, src)
            }
        }

        ArmOpcode::StpPre => {
            let frame_size = mf.frame.size as i64;
            let stp_offset = frame_size - 16;
            // The `sub sp, sp, #N` portion handles N > 4095 via
            // x16 synthesis (audit6 BLOCKING-5 root cause). The
            // `stp ... [sp, #stp_offset]` form is also bounded
            // (signed 7-bit immediate * 8 = ±504 byte range), so
            // we fall back to two `str` instructions when over.
            // For very large frames (stp_offset > 32760, the
            // signed 12-bit max for 64-bit ldr/str unsigned imm),
            // we'd need a register-form load/store — not yet
            // exercised in any test, so the panic catches it.
            let sub_sp = fmt_sp_imm("sub", "sp", "sp", frame_size);
            if stp_offset <= 504 {
                format!("{}\n    stp x29, x30, [sp, #{}]", sub_sp, stp_offset)
            } else if stp_offset <= 32760 {
                format!("{}\n    str x29, [sp, #{}]\n    str x30, [sp, #{}]",
                    sub_sp, stp_offset, stp_offset + 8)
            } else {
                // Frame too large for any ldr/str unsigned immediate.
                // Synthesize the address in x9 (caller-saved scratch)
                // then use register-offset str.
                let x9_addr = fmt_sp_imm("add", "x9", "sp", stp_offset);
                format!("{}\n    {}\n    str x29, [x9]\n    str x30, [x9, #8]",
                    sub_sp, x9_addr)
            }
        }
        ArmOpcode::LdpPost => {
            let frame_size = mf.frame.size as i64;
            let ldp_offset = frame_size - 16;
            let add_sp = fmt_sp_imm("add", "sp", "sp", frame_size);
            if ldp_offset <= 504 {
                format!("ldp x29, x30, [sp, #{}]\n    {}", ldp_offset, add_sp)
            } else if ldp_offset <= 32760 {
                format!("ldr x29, [sp, #{}]\n    ldr x30, [sp, #{}]\n    {}",
                    ldp_offset, ldp_offset + 8, add_sp)
            } else {
                // Frame too large for unsigned immediate ldr.
                // Synthesize address in x9 then restore with register-offset ldr.
                let x9_addr = fmt_sp_imm("add", "x9", "sp", ldp_offset);
                format!("{}\n    ldr x29, [x9]\n    ldr x30, [x9, #8]\n    {}",
                    x9_addr, add_sp)
            }
        }

        // Non-preindex STP/LDP for callee-save pairs.
        // Operands: [src1/dst1, src2/dst2, base, imm].
        ArmOpcode::StpOffset => {
            let r1  = op_str(&inst.operands[0]);
            let r2  = op_str(&inst.operands[1]);
            let base = op_str(&inst.operands[2]);
            let off  = match &inst.operands[3] {
                MachineOperand::Imm(v)        => *v,
                MachineOperand::FrameSlot(v)  => *v as i64,
                _ => 0,
            };
            // Detect FP vs GP to pick correct mnemonic.
            let mnemonic = if r1.starts_with('d') || r1.starts_with('s') { "stp" } else { "stp" };
            format!("{} {}, {}, [{}, #{}]", mnemonic, r1, r2, base, off)
        }
        ArmOpcode::LdpOffset => {
            let r1  = op_str(&inst.operands[0]);
            let r2  = op_str(&inst.operands[1]);
            let base = op_str(&inst.operands[2]);
            let off  = match &inst.operands[3] {
                MachineOperand::Imm(v)        => *v,
                MachineOperand::FrameSlot(v)  => *v as i64,
                _ => 0,
            };
            format!("ldp {}, {}, [{}, #{}]", r1, r2, base, off)
        }

        ArmOpcode::AdrpLdr => {
            if let MachineOperand::ConstPool(idx) = &inst.operands[1] {
                let label = const_pool_label(&mf.name, *idx);
                let dest = op_str(&inst.operands[0]);
                // ADRP requires a GP register. If dest is FP (s/d), use x8 as scratch.
                let is_fp = dest.starts_with('s') || dest.starts_with('d');
                if is_fp {
                    format!("adrp x8, {1}@PAGE\n    ldr {0}, [x8, {1}@PAGEOFF]",
                        dest, label)
                } else {
                    format!("adrp {0}, {1}@PAGE\n    ldr {0}, [{0}, {1}@PAGEOFF]",
                        dest, label)
                }
            } else {
                "nop ; bad adrp+ldr".into()
            }
        }
        ArmOpcode::AdrpAdd => {
            let dest = op_str(&inst.operands[0]);
            match &inst.operands[1] {
                MachineOperand::ConstPool(idx) => {
                    let label = const_pool_label(&mf.name, *idx);
                    format!("adrp {0}, {1}@PAGE\n    add {0}, {0}, {1}@PAGEOFF",
                        dest, label)
                }
                MachineOperand::GlobalLabel(name) => {
                    // Mach-O convention: globals get an underscore prefix.
                    let sym = if name.starts_with('_') {
                        name.clone()
                    } else {
                        format!("_{}", name)
                    };
                    format!("adrp {0}, {1}@PAGE\n    add {0}, {0}, {1}@PAGEOFF",
                        dest, sym)
                }
                _ => "nop ; bad adrp+add".into(),
            }
        }

        ArmOpcode::B => {
            match &inst.operands[0] {
                MachineOperand::BlockRef(id) => format!("b {}", mf.block(*id).label),
                // Tail call to an external symbol (TCO): B _callee
                MachineOperand::Extern(name) => {
                    if name.starts_with('_') {
                        format!("b {}", name)
                    } else {
                        format!("b _{}", name)
                    }
                }
                _ => "b ???".into(),
            }
        }
        ArmOpcode::BCond => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[0] { cond_str(*c) } else { "eq" };
            let target = if let MachineOperand::BlockRef(id) = &inst.operands[1] {
                mf.block(*id).label.clone()
            } else { "???".into() };
            format!("b.{} {}", cond, target)
        }
        ArmOpcode::Bl => {
            if let MachineOperand::Extern(name) = &inst.operands[0] {
                // Mach-O convention: C symbols get a _ prefix.
                if name.starts_with('_') {
                    format!("bl {}", name) // already prefixed
                } else {
                    format!("bl _{}", name) // add Mach-O prefix
                }
            } else { "bl ???".into() }
        }
        ArmOpcode::Sxtw => format!("sxtw {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::Ret => "ret".into(),
        ArmOpcode::Nop => "nop".into(),
        ArmOpcode::Brk => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[0] { *v } else { 1 };
            format!("brk #{}", imm)
        }
    }
}

/// Format a machine operand as assembly text.
fn op_str(op: &MachineOperand) -> String {
    match op {
        MachineOperand::VReg(id) => format!("v{}", id.0), // placeholder until regalloc
        MachineOperand::PhysReg(PhysReg::Sp) => "sp".into(),
        MachineOperand::PhysReg(PhysReg::Xzr) => "xzr".into(),
        MachineOperand::PhysReg(PhysReg::Wzr) => "wzr".into(),
        MachineOperand::PhysReg(PhysReg::Gp(n)) => format!("x{}", n),
        MachineOperand::PhysReg(PhysReg::Gp32(n)) => format!("w{}", n),
        MachineOperand::PhysReg(PhysReg::Fp(n)) => format!("d{}", n),
        MachineOperand::PhysReg(PhysReg::Fp32(n)) => format!("s{}", n),
        MachineOperand::Imm(v) => format!("#{}", v),
        MachineOperand::FrameSlot(off) => format!("[fp, #{}]", off),
        MachineOperand::Cond(c) => cond_str(*c).into(),
        MachineOperand::BlockRef(id) => format!("bb{}", id.0),
        MachineOperand::Extern(name) => name.clone(),
        MachineOperand::GlobalLabel(name) => {
            if name.starts_with('_') { name.clone() } else { format!("_{}", name) }
        }
        MachineOperand::ConstPool(idx) => format!("cp{}", idx),
        MachineOperand::Shift(s) => format!("lsl #{}", s),
    }
}

fn cond_str(c: ArmCond) -> &'static str {
    match c {
        ArmCond::Eq => "eq", ArmCond::Ne => "ne",
        ArmCond::Hs => "hs", ArmCond::Lo => "lo",
        ArmCond::Mi => "mi", ArmCond::Pl => "pl",
        ArmCond::Hi => "hi", ArmCond::Ls => "ls",
        ArmCond::Ge => "ge", ArmCond::Lt => "lt",
        ArmCond::Gt => "gt", ArmCond::Le => "le",
    }
}

/// Generate a constant pool label.
fn const_pool_label(func: &str, idx: u32) -> String {
    format!("__{}_cp{}", func, idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::*;
    use crate::ir::inst::*;
    use crate::ir::builder::FuncBuilder;
    use crate::codegen::isel::select_function;

    fn emit_simple(build: impl FnOnce(&mut FuncBuilder)) -> String {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            build(&mut b);
        }
        let mf = select_function(&func);
        emit_function(&mf)
    }

    #[test]
    fn emit_prologue_epilogue() {
        let asm = emit_simple(|b| b.ret_void());
        assert!(asm.contains("sub sp, sp,"), "missing frame allocation: {}", asm);
        assert!(asm.contains("stp x29, x30, [sp,"), "missing prologue save: {}", asm);
        assert!(asm.contains("ldp x29, x30, [sp,"), "missing epilogue restore: {}", asm);
        assert!(asm.contains("add sp, sp,"), "missing frame deallocation: {}", asm);
        assert!(asm.contains("ret"), "missing ret: {}", asm);
    }

    #[test]
    fn emit_integer_add() {
        let asm = emit_simple(|b| {
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        });
        assert!(asm.contains("add "), "missing add: {}", asm);
    }

    #[test]
    fn emit_function_label() {
        let asm = emit_simple(|b| b.ret_void());
        assert!(asm.contains(".globl _test"), "missing .globl: {}", asm);
        assert!(asm.contains("_test:"), "missing function label: {}", asm);
    }

    #[test]
    fn emit_branch() {
        let asm = emit_simple(|b| {
            let cond = b.const_bool(true);
            let bb_t = b.create_block("then");
            let bb_f = b.create_block("else");
            b.cond_branch(cond, bb_t, vec![], bb_f, vec![]);
            b.set_block(bb_t);
            b.ret_void();
            b.set_block(bb_f);
            b.ret_void();
        });
        assert!(asm.contains("b.ne"), "missing conditional branch: {}", asm);
        assert!(asm.contains("then_"), "missing then label: {}", asm);
        assert!(asm.contains("else_"), "missing else label: {}", asm);
    }
}
