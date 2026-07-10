//! Assembly text emission — converts Machine IR to ARM64 assembly text.
//!
//! Produces output compatible with both afs-as and Apple's system assembler.

use super::mir::*;
use std::fmt::Write;

/// Emit a machine function as ARM64 assembly text.
pub fn emit_function(mf: &MachineFunction) -> String {
    let mut out = String::new();

    // Function directive.
    if mf.internal_only {
        writeln!(out, ".private_extern _{}", mf.name).unwrap();
    } else {
        writeln!(out, ".globl _{}", mf.name).unwrap();
    }
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
                    // Emit as hex integer to avoid decimal expansion issues
                    // with large/small floats that the assembler can't parse.
                    writeln!(out, "    .long 0x{:08x}", v.to_bits()).unwrap();
                }
                ConstPoolEntry::F64(v) => {
                    writeln!(out, ".p2align 3").unwrap();
                    writeln!(out, "{}:", label).unwrap();
                    writeln!(out, "    .quad 0x{:016x}", v.to_bits()).unwrap();
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

fn fmt_stack_alloc(frame_size: i64) -> String {
    // Apple Silicon uses large guard pages, so jumping the stack pointer
    // down by a huge frame in one shot can skip the guard and fault on the
    // first real touch. Probe the stack one chunk at a time for large
    // frames to keep growth fault-safe.
    const STACK_PROBE_STRIDE: i64 = 16 * 1024;

    if frame_size <= STACK_PROBE_STRIDE {
        return fmt_sp_imm("sub", "sp", "sp", frame_size);
    }

    let mut lines = Vec::new();
    let mut remaining = frame_size;
    while remaining > 0 {
        let step = remaining.min(STACK_PROBE_STRIDE);
        lines.push(fmt_sp_imm("sub", "sp", "sp", step));
        lines.push("str xzr, [sp]".to_string());
        remaining -= step;
    }
    lines.join("\n    ")
}

fn fmt_u64_imm(reg: &str, value: u64) -> String {
    let mut parts = Vec::new();
    for shift in [0u32, 16, 32, 48] {
        let chunk = ((value >> shift) & 0xFFFF) as u16;
        if chunk == 0 && !parts.is_empty() {
            continue;
        }
        if parts.is_empty() {
            parts.push(format!("movz {}, #{}", reg, chunk));
        } else {
            parts.push(format!("movk {}, #{}, lsl #{}", reg, chunk, shift));
        }
    }
    if parts.is_empty() {
        format!("movz {}, #0", reg)
    } else {
        parts.join("\n    ")
    }
}

fn fmt_addr_with_offset(dest: &str, base: &str, offset: i64, scratch: &str) -> String {
    if offset == 0 {
        return format!("mov {}, {}", dest, base);
    }

    if (0..=4095).contains(&offset) {
        return format!("add {}, {}, #{}", dest, base, offset);
    }
    if (-4095..=-1).contains(&offset) {
        return format!("sub {}, {}, #{}", dest, base, -offset);
    }

    let imm = fmt_u64_imm(scratch, offset.unsigned_abs());
    let op = if offset.is_negative() { "sub" } else { "add" };
    format!("{}\n    {} {}, {}, {}", imm, op, dest, base, scratch)
}

/// Emit a single machine instruction as assembly text. Public so the
/// branch-relaxation pass can count emit-time instruction bytes
/// directly rather than re-deriving each opcode's expansion rules.
pub fn emit_inst_text(inst: &MachineInst, mf: &MachineFunction) -> String {
    emit_inst(inst, mf)
}

/// Emit a single machine instruction as assembly text.
fn emit_inst(inst: &MachineInst, mf: &MachineFunction) -> String {
    match inst.opcode {
        ArmOpcode::AddReg => format!(
            "add {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::AddsReg => format!(
            "adds {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::AdcReg => format!(
            "adc {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
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
                _ => return format!("add {}, {}, {}", dest, base, op_str(&inst.operands[2])),
            };
            // Both `add x29, sp, #N` (FP setup) and `add Xd, Xn, #N`
            // need the > 4095 fallback. Use the same scratch
            // synthesis since x16 is safe in the prologue.
            fmt_sp_imm("add", &dest, &base, imm)
        }
        ArmOpcode::SubReg => format!(
            "sub {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::SubsReg => format!(
            "subs {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::SbcReg => format!(
            "sbc {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
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
        ArmOpcode::Mul => format!(
            "mul {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::Sdiv => format!(
            "sdiv {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::Madd => format!(
            "madd {}, {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2]),
            op_str(&inst.operands[3])
        ),
        ArmOpcode::Msub => format!(
            "msub {}, {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2]),
            op_str(&inst.operands[3])
        ),
        ArmOpcode::Neg => format!(
            "neg {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),

        ArmOpcode::AndReg => format!(
            "and {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::OrrReg => format!(
            "orr {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::EorReg => format!(
            "eor {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::OrnReg => format!(
            "orn {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::LslReg => format!(
            "lsl {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::LsrReg => format!(
            "lsr {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::AsrReg => format!(
            "asr {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),

        ArmOpcode::Mvn => format!(
            "mvn {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::Clz => format!(
            "clz {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::Rbit => format!(
            "rbit {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),

        ArmOpcode::CmpReg => format!(
            "cmp {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::CmpImm => format!(
            "cmp {}, #{}",
            op_str(&inst.operands[0]),
            if let MachineOperand::Imm(v) = &inst.operands[1] {
                *v
            } else {
                0
            }
        ),
        ArmOpcode::Cset | ArmOpcode::FCset => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[1] {
                cond_str(*c)
            } else {
                "eq"
            };
            format!("cset {}, {}", op_str(&inst.operands[0]), cond)
        }
        ArmOpcode::CselReg => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[3] {
                cond_str(*c)
            } else {
                "eq"
            };
            format!(
                "csel {}, {}, {}, {}",
                op_str(&inst.operands[0]),
                op_str(&inst.operands[1]),
                op_str(&inst.operands[2]),
                cond
            )
        }
        ArmOpcode::FCmpReg => format!(
            "fcmp {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::FcselReg => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[3] {
                cond_str(*c)
            } else {
                "eq"
            };
            format!(
                "fcsel {}, {}, {}, {}",
                op_str(&inst.operands[0]),
                op_str(&inst.operands[1]),
                op_str(&inst.operands[2]),
                cond
            )
        }

        ArmOpcode::FaddS | ArmOpcode::FaddD => format!(
            "fadd {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::FsubS | ArmOpcode::FsubD => format!(
            "fsub {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::FmulS | ArmOpcode::FmulD => format!(
            "fmul {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::FdivS | ArmOpcode::FdivD => format!(
            "fdiv {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2])
        ),
        ArmOpcode::FnegS | ArmOpcode::FnegD => format!(
            "fneg {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::FabsS | ArmOpcode::FabsD => format!(
            "fabs {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::FsqrtS | ArmOpcode::FsqrtD => format!(
            "fsqrt {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        // Fused multiply-add/subtract: 4-operand (dest, Sn, Sm, Sa).
        // FMADD  Sd, Sn, Sm, Sa → Sd = Sa + Sn*Sm
        // FMSUB  Sd, Sn, Sm, Sa → Sd = Sa - Sn*Sm
        // FNMSUB Sd, Sn, Sm, Sa → Sd = Sn*Sm - Sa
        ArmOpcode::FmaddS | ArmOpcode::FmaddD => format!(
            "fmadd {}, {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2]),
            op_str(&inst.operands[3])
        ),
        ArmOpcode::FmsubS | ArmOpcode::FmsubD => format!(
            "fmsub {}, {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2]),
            op_str(&inst.operands[3])
        ),
        ArmOpcode::FnmsubS | ArmOpcode::FnmsubD => format!(
            "fnmsub {}, {}, {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2]),
            op_str(&inst.operands[3])
        ),

        ArmOpcode::ScvtfSW | ArmOpcode::ScvtfDW | ArmOpcode::ScvtfSX | ArmOpcode::ScvtfDX => {
            format!(
                "scvtf {}, {}",
                op_str(&inst.operands[0]),
                op_str(&inst.operands[1])
            )
        }
        ArmOpcode::FcvtzsWS | ArmOpcode::FcvtzsWD | ArmOpcode::FcvtzsXS | ArmOpcode::FcvtzsXD => {
            format!(
                "fcvtzs {}, {}",
                op_str(&inst.operands[0]),
                op_str(&inst.operands[1])
            )
        }
        ArmOpcode::FcvtSD => format!(
            "fcvt {}, {}",
            fp_reg_str(&inst.operands[0], false),
            fp_reg_str(&inst.operands[1], true)
        ),
        ArmOpcode::FcvtDS => format!(
            "fcvt {}, {}",
            fp_reg_str(&inst.operands[0], true),
            fp_reg_str(&inst.operands[1], false)
        ),

        ArmOpcode::Movz => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[1] {
                *v
            } else {
                0
            };
            let shift = if let MachineOperand::Shift(s) = &inst.operands[2] {
                *s
            } else {
                0
            };
            if shift == 0 {
                format!("movz {}, #{}", op_str(&inst.operands[0]), imm)
            } else {
                format!(
                    "movz {}, #{}, lsl #{}",
                    op_str(&inst.operands[0]),
                    imm,
                    shift
                )
            }
        }
        ArmOpcode::Movk => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[1] {
                *v
            } else {
                0
            };
            let shift = if let MachineOperand::Shift(s) = &inst.operands[2] {
                *s
            } else {
                0
            };
            format!(
                "movk {}, #{}, lsl #{}",
                op_str(&inst.operands[0]),
                imm,
                shift
            )
        }
        ArmOpcode::Movn => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[1] {
                *v
            } else {
                0
            };
            let shift = if let MachineOperand::Shift(s) = &inst.operands[2] {
                *s
            } else {
                0
            };
            format!(
                "movn {}, #{}, lsl #{}",
                op_str(&inst.operands[0]),
                imm,
                shift
            )
        }
        ArmOpcode::MovReg => {
            let dest = op_str(&inst.operands[0]);
            let src = op_str(&inst.operands[1]);
            // Handle width mismatch: w→x extend or x→w truncate.
            let dest_is_x = dest.starts_with('x');
            let dest_is_w = dest.starts_with('w');
            let src_is_w = src.starts_with('w');
            let src_is_x = src.starts_with('x');
            // Cross-register-class move: AArch64 `mov` only encodes GP↔GP
            // (and FP↔FP via FmovReg). When register-allocation hands us
            // a MovReg straddling classes, emit `fmov` which transfers
            // bits between an integer GPR and an SIMD/FP register.
            let dest_is_gp = dest_is_x || dest_is_w;
            let src_is_gp = src_is_x || src_is_w;
            let dest_is_fp = dest.starts_with('s') || dest.starts_with('d');
            let src_is_fp = src.starts_with('s') || src.starts_with('d');
            if dest_is_gp && src_is_fp {
                // GPR ← FPR: pick GPR width to match FPR (s→w, d→x).
                let gp = if src.starts_with('d') {
                    if dest_is_x {
                        dest.clone()
                    } else {
                        format!("x{}", &dest[1..])
                    }
                } else {
                    if dest_is_w {
                        dest.clone()
                    } else {
                        format!("w{}", &dest[1..])
                    }
                };
                return format!("fmov {}, {}", gp, src);
            }
            if dest_is_fp && src_is_gp {
                let gp = if dest.starts_with('d') {
                    if src_is_x {
                        src.clone()
                    } else {
                        format!("x{}", &src[1..])
                    }
                } else {
                    if src_is_w {
                        src.clone()
                    } else {
                        format!("w{}", &src[1..])
                    }
                };
                return format!("fmov {}, {}", dest, gp);
            }
            if dest_is_x && src_is_w {
                // Zero-extend 32→64: use uxtw.
                format!("uxtw {}, {}", dest, src)
            } else if dest_is_w && src_is_x {
                // Truncate 64→32 by reading the source register through its
                // 32-bit view. `mov wN, xM` is not a valid AArch64 encoding.
                format!("mov {}, w{}", dest, &src[1..])
            } else {
                format!("mov {}, {}", dest, src)
            }
        }
        ArmOpcode::FmovReg => format!(
            "fmov {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::Mov16B => format!(
            "mov.16b {}, {}",
            v_reg_bare(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::AddpV2D => format!(
            "addp.2d {}, {}, {}",
            v_reg_bare(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
            v_reg_bare(&inst.operands[2]),
        ),
        ArmOpcode::FaddpV4S => format!(
            "faddp.4s {}, {}, {}",
            v_reg_bare(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
            v_reg_bare(&inst.operands[2]),
        ),

        ArmOpcode::LdrImm | ArmOpcode::LdrFpImm | ArmOpcode::LdrsbImm | ArmOpcode::LdrshImm => {
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
                format!(
                    "{}\n    {} {}, [x8]",
                    fmt_addr_with_offset("x8", &base, offset_val, "x16"),
                    mnemonic,
                    dest
                )
            }
        }
        ArmOpcode::StrImm | ArmOpcode::StrFpImm | ArmOpcode::StrbImm | ArmOpcode::StrhImm => {
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
                format!(
                    "{}\n    {} {}, [x8]",
                    fmt_addr_with_offset("x8", &base, offset_val, "x16"),
                    mnemonic,
                    src
                )
            }
        }
        // Sprint 05: scaled-register-offset addressing. Operands are
        // [dest, base, idx, Imm(shift)]. Shift 0 elides the `, lsl
        // #0` suffix per the assembler convention.
        ArmOpcode::LdrReg | ArmOpcode::LdrFpReg | ArmOpcode::StrReg | ArmOpcode::StrFpReg => {
            let dest = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            let idx = op_str(&inst.operands[2]);
            let shift = match &inst.operands[3] {
                MachineOperand::Imm(v) => *v,
                _ => 0,
            };
            let mnemonic = match inst.opcode {
                ArmOpcode::LdrReg | ArmOpcode::LdrFpReg => "ldr",
                ArmOpcode::StrReg | ArmOpcode::StrFpReg => "str",
                _ => unreachable!(),
            };
            if shift == 0 {
                format!("{} {}, [{}, {}]", mnemonic, dest, base, idx)
            } else {
                format!("{} {}, [{}, {}, lsl #{}]", mnemonic, dest, base, idx, shift)
            }
        }

        ArmOpcode::StpPre => {
            let frame_size = mf.frame.size as i64;
            let stp_offset = frame_size - 16;
            // The `sub sp, sp, #N` portion handles N > 4095 via
            // x16 synthesis (audit6 BLOCKING-5 root cause), and
            // probes very large frames so macOS guard pages aren't
            // skipped in one jump. The `stp ... [sp, #stp_offset]`
            // form is also bounded
            // (signed 7-bit immediate * 8 = ±504 byte range), so
            // we fall back to two `str` instructions when over.
            // For very large frames (stp_offset > 32760, the
            // signed 12-bit max for 64-bit ldr/str unsigned imm),
            // we'd need a register-form load/store — not yet
            // exercised in any test, so the panic catches it.
            let sub_sp = fmt_stack_alloc(frame_size);
            if stp_offset <= 504 {
                format!("{}\n    stp x29, x30, [sp, #{}]", sub_sp, stp_offset)
            } else if stp_offset <= 32760 {
                format!(
                    "{}\n    str x29, [sp, #{}]\n    str x30, [sp, #{}]",
                    sub_sp,
                    stp_offset,
                    stp_offset + 8
                )
            } else {
                // Frame too large for any ldr/str unsigned immediate.
                // Synthesize the address in x9 (caller-saved scratch)
                // then use register-offset str.
                let x9_addr = fmt_sp_imm("add", "x9", "sp", stp_offset);
                format!(
                    "{}\n    {}\n    str x29, [x9]\n    str x30, [x9, #8]",
                    sub_sp, x9_addr
                )
            }
        }
        ArmOpcode::LdpPost => {
            let frame_size = mf.frame.size as i64;
            let ldp_offset = frame_size - 16;
            let add_sp = fmt_sp_imm("add", "sp", "sp", frame_size);
            if ldp_offset <= 504 {
                format!("ldp x29, x30, [sp, #{}]\n    {}", ldp_offset, add_sp)
            } else if ldp_offset <= 32760 {
                format!(
                    "ldr x29, [sp, #{}]\n    ldr x30, [sp, #{}]\n    {}",
                    ldp_offset,
                    ldp_offset + 8,
                    add_sp
                )
            } else {
                // Frame too large for unsigned immediate ldr.
                // Synthesize address in x9 then restore with register-offset ldr.
                let x9_addr = fmt_sp_imm("add", "x9", "sp", ldp_offset);
                format!(
                    "{}\n    ldr x29, [x9]\n    ldr x30, [x9, #8]\n    {}",
                    x9_addr, add_sp
                )
            }
        }

        // Non-preindex STP/LDP for callee-save pairs.
        // Operands: [src1/dst1, src2/dst2, base, imm].
        ArmOpcode::StpOffset => {
            let r1 = op_str(&inst.operands[0]);
            let r2 = op_str(&inst.operands[1]);
            let base = op_str(&inst.operands[2]);
            let off = match &inst.operands[3] {
                MachineOperand::Imm(v) => *v,
                MachineOperand::FrameSlot(v) => *v as i64,
                _ => 0,
            };
            // STP signed-offset range: 7-bit signed × 8 → [-512, 504].
            // Fall back to two individual STR instructions if out of range.
            if (-512..=504).contains(&off) {
                format!("stp {}, {}, [{}, #{}]", r1, r2, base, off)
            } else {
                format!(
                    "{}\n    str {}, [x9]\n    str {}, [x9, #8]",
                    fmt_addr_with_offset("x9", &base, off, "x16"),
                    r1,
                    r2
                )
            }
        }
        ArmOpcode::LdpOffset => {
            let r1 = op_str(&inst.operands[0]);
            let r2 = op_str(&inst.operands[1]);
            let base = op_str(&inst.operands[2]);
            let off = match &inst.operands[3] {
                MachineOperand::Imm(v) => *v,
                MachineOperand::FrameSlot(v) => *v as i64,
                _ => 0,
            };
            // LDP signed-offset range: 7-bit signed × 8 → [-512, 504].
            // Fall back to two individual LDR instructions if out of range.
            if (-512..=504).contains(&off) {
                format!("ldp {}, {}, [{}, #{}]", r1, r2, base, off)
            } else {
                format!(
                    "{}\n    ldr {}, [x9]\n    ldr {}, [x9, #8]",
                    fmt_addr_with_offset("x9", &base, off, "x16"),
                    r1,
                    r2
                )
            }
        }

        ArmOpcode::AdrpLdr => {
            if let MachineOperand::ConstPool(idx) = &inst.operands[1] {
                let label = const_pool_label(&mf.name, *idx);
                let dest = op_str(&inst.operands[0]);
                // ADRP requires a GP register. If dest is FP (s/d), use x8 as scratch.
                let is_fp = dest.starts_with('s') || dest.starts_with('d');
                if is_fp {
                    format!(
                        "adrp x8, {1}@PAGE\n    ldr {0}, [x8, {1}@PAGEOFF]",
                        dest, label
                    )
                } else {
                    format!(
                        "adrp {0}, {1}@PAGE\n    ldr {0}, [{0}, {1}@PAGEOFF]",
                        dest, label
                    )
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
                    format!(
                        "adrp {0}, {1}@PAGE\n    add {0}, {0}, {1}@PAGEOFF",
                        dest, label
                    )
                }
                MachineOperand::GlobalLabel(name) => {
                    // Mach-O convention: globals get an underscore prefix.
                    let sym = if name.starts_with('_') {
                        name.clone()
                    } else {
                        format!("_{}", name)
                    };
                    format!(
                        "adrp {0}, {1}@PAGE\n    add {0}, {0}, {1}@PAGEOFF",
                        dest, sym
                    )
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
            let cond = if let MachineOperand::Cond(c) = &inst.operands[0] {
                cond_str(*c)
            } else {
                "eq"
            };
            let target = if let MachineOperand::BlockRef(id) = &inst.operands[1] {
                mf.block(*id).label.clone()
            } else {
                "???".into()
            };
            format!("b.{} {}", cond, target)
        }
        ArmOpcode::Cbz | ArmOpcode::Cbnz => {
            let mnemonic = match inst.opcode {
                ArmOpcode::Cbz => "cbz",
                _ => "cbnz",
            };
            let target = if let MachineOperand::BlockRef(id) = &inst.operands[1] {
                mf.block(*id).label.clone()
            } else {
                "???".into()
            };
            format!("{} {}, {}", mnemonic, op_str(&inst.operands[0]), target)
        }
        ArmOpcode::Tbz | ArmOpcode::Tbnz => {
            let mnemonic = match inst.opcode {
                ArmOpcode::Tbz => "tbz",
                _ => "tbnz",
            };
            let bit = if let MachineOperand::Imm(v) = &inst.operands[1] {
                *v
            } else {
                0
            };
            let target = if let MachineOperand::BlockRef(id) = &inst.operands[2] {
                mf.block(*id).label.clone()
            } else {
                "???".into()
            };
            format!(
                "{} {}, #{}, {}",
                mnemonic,
                op_str(&inst.operands[0]),
                bit,
                target
            )
        }
        ArmOpcode::Bl => {
            if let MachineOperand::Extern(name) = &inst.operands[0] {
                // Mach-O convention: C symbols get a _ prefix.
                if name.starts_with('_') {
                    format!("bl {}", name) // already prefixed
                } else {
                    format!("bl _{}", name) // add Mach-O prefix
                }
            } else {
                "bl ???".into()
            }
        }
        ArmOpcode::Blr => format!("blr {}", op_str(&inst.operands[0])),
        ArmOpcode::Sxtw => format!(
            "sxtw {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::Sxth => format!(
            "sxth {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::Sxtb => format!(
            "sxtb {}, {}",
            op_str(&inst.operands[0]),
            op_str(&inst.operands[1])
        ),
        ArmOpcode::Ret => "ret".into(),
        ArmOpcode::Nop => "nop".into(),
        ArmOpcode::Brk => {
            let imm = if let MachineOperand::Imm(v) = &inst.operands[0] {
                *v
            } else {
                1
            };
            format!("brk #{}", imm)
        }

        // ---- NEON SIMD vector ops (Sprint 12 Stage 2) ----
        //
        // Each op forwards to a small helper so the lane-shape suffix
        // (.4s / .2d / .s[n] / .d[n]) lives in one place.
        ArmOpcode::AddV4S => fmt_vbinop(inst, "add", "4s"),
        ArmOpcode::AddV2D => fmt_vbinop(inst, "add", "2d"),
        ArmOpcode::SubV4S => fmt_vbinop(inst, "sub", "4s"),
        ArmOpcode::SubV2D => fmt_vbinop(inst, "sub", "2d"),
        ArmOpcode::MulV4S => fmt_vbinop(inst, "mul", "4s"),
        ArmOpcode::NegV4S => fmt_vunop(inst, "neg", "4s"),
        ArmOpcode::NegV2D => fmt_vunop(inst, "neg", "2d"),
        ArmOpcode::AbsV4S => fmt_vunop(inst, "abs", "4s"),
        ArmOpcode::AbsV2D => fmt_vunop(inst, "abs", "2d"),
        ArmOpcode::FaddV4S => fmt_vbinop(inst, "fadd", "4s"),
        ArmOpcode::FaddV2D => fmt_vbinop(inst, "fadd", "2d"),
        ArmOpcode::FsubV4S => fmt_vbinop(inst, "fsub", "4s"),
        ArmOpcode::FsubV2D => fmt_vbinop(inst, "fsub", "2d"),
        ArmOpcode::FmulV4S => fmt_vbinop(inst, "fmul", "4s"),
        ArmOpcode::FmulV2D => fmt_vbinop(inst, "fmul", "2d"),
        ArmOpcode::FdivV4S => fmt_vbinop(inst, "fdiv", "4s"),
        ArmOpcode::FdivV2D => fmt_vbinop(inst, "fdiv", "2d"),
        ArmOpcode::FnegV4S => fmt_vunop(inst, "fneg", "4s"),
        ArmOpcode::FnegV2D => fmt_vunop(inst, "fneg", "2d"),
        ArmOpcode::FabsV4S => fmt_vunop(inst, "fabs", "4s"),
        ArmOpcode::FabsV2D => fmt_vunop(inst, "fabs", "2d"),
        ArmOpcode::FsqrtV4S => fmt_vunop(inst, "fsqrt", "4s"),
        ArmOpcode::FsqrtV2D => fmt_vunop(inst, "fsqrt", "2d"),
        ArmOpcode::BslV16B => fmt_vbinop(inst, "bsl", "16b"),
        ArmOpcode::FcmgtV4S => fmt_vbinop(inst, "fcmgt", "4s"),
        ArmOpcode::FcmgtV2D => fmt_vbinop(inst, "fcmgt", "2d"),
        ArmOpcode::FcmgeV4S => fmt_vbinop(inst, "fcmge", "4s"),
        ArmOpcode::FcmgeV2D => fmt_vbinop(inst, "fcmge", "2d"),
        ArmOpcode::FcmeqV4S => fmt_vbinop(inst, "fcmeq", "4s"),
        ArmOpcode::FcmeqV2D => fmt_vbinop(inst, "fcmeq", "2d"),
        ArmOpcode::CmgtV4S => fmt_vbinop(inst, "cmgt", "4s"),
        ArmOpcode::CmgeV4S => fmt_vbinop(inst, "cmge", "4s"),
        ArmOpcode::CmeqV4S => fmt_vbinop(inst, "cmeq", "4s"),
        ArmOpcode::FmlaV4S => fmt_vbinop(inst, "fmla", "4s"),
        ArmOpcode::FmlaV2D => fmt_vbinop(inst, "fmla", "2d"),
        ArmOpcode::FminV4S => fmt_vbinop(inst, "fmin", "4s"),
        ArmOpcode::FminV2D => fmt_vbinop(inst, "fmin", "2d"),
        ArmOpcode::FmaxV4S => fmt_vbinop(inst, "fmax", "4s"),
        ArmOpcode::FmaxV2D => fmt_vbinop(inst, "fmax", "2d"),
        ArmOpcode::SminV4S => fmt_vbinop(inst, "smin", "4s"),
        ArmOpcode::SmaxV4S => fmt_vbinop(inst, "smax", "4s"),
        ArmOpcode::UminV4S => fmt_vbinop(inst, "umin", "4s"),
        ArmOpcode::UmaxV4S => fmt_vbinop(inst, "umax", "4s"),

        // afs-as dialect: cross-lane reductions encode the shape in
        // the mnemonic suffix; the destination is a scalar `s/d` and
        // the source is the bare vector register.
        ArmOpcode::FaddpV2S => format!(
            "faddp.2s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::FaddpV2D => format!(
            "faddp.2d {}, {}",
            fp64_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::Faddv4S => format!(
            "faddv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::Sminv4S => format!(
            "sminv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::Smaxv4S => format!(
            "smaxv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::FmaxvV4S => format!(
            "fmaxv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::FminvV4S => format!(
            "fminv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::FmaxpV2DScalar => format!(
            "fmaxp.2d {}, {}",
            fp64_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::FminpV2DScalar => format!(
            "fminp.2d {}, {}",
            fp64_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::Uminv4S => format!(
            "uminv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::Umaxv4S => format!(
            "umaxv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),
        ArmOpcode::Addv4S => format!(
            "addv.4s {}, {}",
            fp32_scalar(&inst.operands[0]),
            v_reg_bare(&inst.operands[1]),
        ),

        ArmOpcode::DupGen4S => format!(
            "dup.4s {}, {}",
            v_reg_bare(&inst.operands[0]),
            op_str(&inst.operands[1]),
        ),
        ArmOpcode::DupGen2D => format!(
            "dup.2d {}, {}",
            v_reg_bare(&inst.operands[0]),
            op_str(&inst.operands[1]),
        ),
        ArmOpcode::DupEl4S => format!(
            "dup.4s {}, {}",
            v_reg_bare(&inst.operands[0]),
            v_lane_bare(&inst.operands[1], "s", 0),
        ),
        ArmOpcode::DupEl2D => format!(
            "dup.2d {}, {}",
            v_reg_bare(&inst.operands[0]),
            v_lane_bare(&inst.operands[1], "d", 0),
        ),
        ArmOpcode::Ins4S => {
            let lane = imm_u8(&inst.operands[1]);
            format!(
                "ins.s {}, {}",
                v_lane_bare(&inst.operands[0], "s", lane),
                op_str(&inst.operands[2]),
            )
        }
        ArmOpcode::Ins2D => {
            let lane = imm_u8(&inst.operands[1]);
            format!(
                "ins.d {}, {}",
                v_lane_bare(&inst.operands[0], "d", lane),
                op_str(&inst.operands[2]),
            )
        }
        ArmOpcode::Umov4S => {
            let lane = imm_u8(&inst.operands[2]);
            format!(
                "umov.s {}, {}",
                op_str(&inst.operands[0]),
                v_lane_bare(&inst.operands[1], "s", lane),
            )
        }
        ArmOpcode::Umov2D => {
            let lane = imm_u8(&inst.operands[2]);
            format!(
                "umov.d {}, {}",
                op_str(&inst.operands[0]),
                v_lane_bare(&inst.operands[1], "d", lane),
            )
        }
        ArmOpcode::FmovEl4S => {
            let lane = imm_u8(&inst.operands[2]);
            format!(
                "mov.s {}, {}",
                fp32_scalar(&inst.operands[0]),
                v_lane_bare(&inst.operands[1], "s", lane),
            )
        }
        ArmOpcode::FmovEl2D => {
            let lane = imm_u8(&inst.operands[2]);
            format!(
                "mov.d {}, {}",
                fp64_scalar(&inst.operands[0]),
                v_lane_bare(&inst.operands[1], "d", lane),
            )
        }

        ArmOpcode::LdrQ => format!(
            "ldr {}, [{}, {}]",
            q_reg(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2]),
        ),
        ArmOpcode::StrQ => format!(
            "str {}, [{}, {}]",
            q_reg(&inst.operands[0]),
            op_str(&inst.operands[1]),
            op_str(&inst.operands[2]),
        ),
    }
}

// ---- NEON formatting helpers ----

fn v_reg(op: &MachineOperand, shape: &str) -> String {
    match op {
        MachineOperand::VReg(id) => format!("v{}.{}", id.0, shape),
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            format!("v{}.{}", n, shape)
        }
        _ => format!("{}.{}", op_str(op), shape),
    }
}

fn q_reg(op: &MachineOperand) -> String {
    match op {
        MachineOperand::VReg(id) => format!("q{}", id.0),
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            format!("q{}", n)
        }
        _ => format!("q{}", op_str(op)),
    }
}

fn v_lane(op: &MachineOperand, lane_ty: &str, lane: u8) -> String {
    match op {
        MachineOperand::VReg(id) => format!("v{}.{}[{}]", id.0, lane_ty, lane),
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            format!("v{}.{}[{}]", n, lane_ty, lane)
        }
        _ => format!("v{}.{}[{}]", op_str(op), lane_ty, lane),
    }
}

fn fp32_scalar(op: &MachineOperand) -> String {
    match op {
        MachineOperand::VReg(id) => format!("s{}", id.0),
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            format!("s{}", n)
        }
        _ => op_str(op),
    }
}

fn fp64_scalar(op: &MachineOperand) -> String {
    match op {
        MachineOperand::VReg(id) => format!("d{}", id.0),
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            format!("d{}", n)
        }
        _ => op_str(op),
    }
}

fn imm_u8(op: &MachineOperand) -> u8 {
    if let MachineOperand::Imm(v) = op {
        *v as u8
    } else {
        0
    }
}

fn fmt_vbinop(inst: &MachineInst, mnemonic: &str, shape: &str) -> String {
    // afs-as dialect: shape suffix is part of the mnemonic, operand
    // registers are bare (`fadd.4s v0, v1, v2`). Encodes to the same
    // bytes as the Apple/GNU `fadd v0.4s, v1.4s, v2.4s` form.
    format!(
        "{}.{} {}, {}, {}",
        mnemonic,
        shape,
        v_reg_bare(&inst.operands[0]),
        v_reg_bare(&inst.operands[1]),
        v_reg_bare(&inst.operands[2]),
    )
}

fn fmt_vunop(inst: &MachineInst, mnemonic: &str, shape: &str) -> String {
    format!(
        "{}.{} {}, {}",
        mnemonic,
        shape,
        v_reg_bare(&inst.operands[0]),
        v_reg_bare(&inst.operands[1]),
    )
}

fn v_reg_bare(op: &MachineOperand) -> String {
    match op {
        MachineOperand::VReg(id) => format!("v{}", id.0),
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            format!("v{}", n)
        }
        _ => op_str(op),
    }
}

fn v_lane_bare(op: &MachineOperand, _lane_ty: &str, lane: u8) -> String {
    // afs-as dialect for `umov.s w3, v0[2]` — bare reg with `[lane]`
    // suffix; the element-size width is encoded into the mnemonic
    // (`umov.s` / `umov.d`).
    match op {
        MachineOperand::VReg(id) => format!("v{}[{}]", id.0, lane),
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            format!("v{}[{}]", n, lane)
        }
        _ => format!("{}[{}]", op_str(op), lane),
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
            if name.starts_with('_') {
                name.clone()
            } else {
                format!("_{}", name)
            }
        }
        MachineOperand::ConstPool(idx) => format!("cp{}", idx),
        MachineOperand::Shift(s) => format!("lsl #{}", s),
    }
}

fn fp_reg_str(op: &MachineOperand, is_f64: bool) -> String {
    match op {
        MachineOperand::PhysReg(PhysReg::Fp(n)) | MachineOperand::PhysReg(PhysReg::Fp32(n)) => {
            if is_f64 {
                format!("d{}", n)
            } else {
                format!("s{}", n)
            }
        }
        _ => op_str(op),
    }
}

fn cond_str(c: ArmCond) -> &'static str {
    match c {
        ArmCond::Eq => "eq",
        ArmCond::Ne => "ne",
        ArmCond::Hs => "hs",
        ArmCond::Lo => "lo",
        ArmCond::Mi => "mi",
        ArmCond::Pl => "pl",
        ArmCond::Hi => "hi",
        ArmCond::Ls => "ls",
        ArmCond::Ge => "ge",
        ArmCond::Lt => "lt",
        ArmCond::Gt => "gt",
        ArmCond::Le => "le",
    }
}

/// Generate a constant pool label.
fn const_pool_label(func: &str, idx: u32) -> String {
    format!("__{}_cp{}", func, idx)
}

#[cfg(test)]
mod tests {
    fn emit_globals_lp64_test(globals: &[Global]) -> String {
        crate::codegen::shared::emit_globals(
            globals,
            &crate::target::TargetLayout::LP64,
            crate::codegen::shared::GlobalsDialect::MachO,
        )
    }

    use super::*;
    use crate::codegen::isel::select_function;
    use crate::ir::builder::FuncBuilder;
    use crate::ir::inst::*;
    use crate::ir::types::*;

    fn emit_simple(build: impl FnOnce(&mut FuncBuilder)) -> String {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            build(&mut b);
        }
        let mf = select_function(&func, crate::target::TargetLayout::LP64);
        emit_function(&mf)
    }

    #[test]
    fn emit_prologue_epilogue() {
        let asm = emit_simple(|b| b.ret_void());
        assert!(
            asm.contains("sub sp, sp,"),
            "missing frame allocation: {}",
            asm
        );
        assert!(
            asm.contains("stp x29, x30, [sp,"),
            "missing prologue save: {}",
            asm
        );
        assert!(
            asm.contains("ldp x29, x30, [sp,"),
            "missing epilogue restore: {}",
            asm
        );
        assert!(
            asm.contains("add sp, sp,"),
            "missing frame deallocation: {}",
            asm
        );
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

    /// Verify that functions with frame sizes > 4095 use x16 scratch
    /// synthesis for the `sub sp, sp, #N` prologue and `add sp, sp, #N`
    /// epilogue rather than an out-of-range immediate.
    #[test]
    fn emit_large_frame_prologue() {
        // 700 allocas of i64 = 700 * 8 = 5600 bytes, well over 4095.
        let asm = emit_simple(|b| {
            for _ in 0..700 {
                let _ = b.alloca(IrType::Int(IntWidth::I64));
            }
            b.ret_void();
        });
        // The 12-bit immediate max is 4095, so the emitter must
        // synthesize the frame size via x16.
        assert!(
            asm.contains("movz x16,"),
            "large frame should use x16 synthesis: {}",
            asm
        );
        assert!(
            asm.contains("sub sp, sp, x16"),
            "large frame sub should use register form: {}",
            asm
        );
        assert!(
            asm.contains("add sp, sp, x16"),
            "large frame add should use register form: {}",
            asm
        );
        // Must NOT contain a raw "sub sp, sp, #5" that exceeds 4095.
        assert!(
            !asm.contains("sub sp, sp, #5"),
            "should not emit out-of-range immediate: {}",
            asm
        );
    }

    #[test]
    fn emit_huge_frame_with_stack_probes() {
        let asm = emit_simple(|b| {
            for _ in 0..3000 {
                let _ = b.alloca(IrType::Int(IntWidth::I64));
            }
            b.ret_void();
        });
        assert!(
            asm.contains("str xzr, [sp]"),
            "huge frame should probe each chunk: {}",
            asm
        );
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

    #[test]
    fn emit_i128_scalar_global_as_two_quads() {
        let asm = emit_globals_lp64_test(&[Global {
            name: "big".into(),
            ty: IrType::Int(IntWidth::I128),
            initializer: Some(GlobalInit::Int(18_446_744_073_709_551_616i128)),
        }]);

        assert!(
            asm.contains(".section __DATA,__data"),
            "missing data section:\n{}",
            asm
        );
        assert!(
            asm.contains(".private_extern _big"),
            "missing global symbol:\n{}",
            asm
        );
        assert!(
            asm.contains(".p2align 4"),
            "i128 globals need 16-byte alignment:\n{}",
            asm
        );
        assert_eq!(
            asm.matches(".quad").count(),
            2,
            "scalar i128 should emit two quads:\n{}",
            asm
        );
        assert!(
            asm.contains(".quad 0x0000000000000000\n    .quad 0x0000000000000001"),
            "scalar i128 should emit low/high 64-bit words in memory order:\n{}",
            asm
        );
    }

    #[test]
    fn emit_i128_array_global_as_word_pairs() {
        let asm = emit_globals_lp64_test(&[Global {
            name: "arr".into(),
            ty: IrType::Array(Box::new(IrType::Int(IntWidth::I128)), 2),
            initializer: Some(GlobalInit::IntArray(vec![1, -1])),
        }]);

        assert_eq!(
            asm.matches(".quad").count(),
            4,
            "two i128 elements should emit four quads:\n{}",
            asm
        );
        assert!(
            asm.contains(".quad 0x0000000000000001\n    .quad 0x0000000000000000"),
            "positive i128 array element should preserve low/high word order:\n{}",
            asm
        );
        assert!(
            asm.contains(".quad 0xffffffffffffffff\n    .quad 0xffffffffffffffff"),
            "negative i128 array element should preserve two's-complement words:\n{}",
            asm
        );
    }

    #[test]
    fn emit_vtable_quad_table_machos_symbols_get_underscore_prefix() {
        let asm = emit_globals_lp64_test(&[Global {
            name: "afs_vtable_m_t".into(),
            ty: IrType::Array(Box::new(IrType::Int(IntWidth::I64)), 3),
            initializer: Some(GlobalInit::QuadTable(vec![
                QuadSlot::Int(42),
                QuadSlot::Int(0),
                QuadSlot::Sym("afs_modproc_m_method".into()),
            ])),
        }]);
        // Vtables are externally visible (consumers in other TUs link to
        // them) and 8-byte aligned.
        assert!(
            asm.contains(".globl _afs_vtable_m_t"),
            "vtable needs external linkage:\n{}",
            asm
        );
        assert!(
            asm.contains(".p2align 3\n_afs_vtable_m_t:"),
            "vtable needs 8-byte alignment:\n{}",
            asm
        );
        // Header ints emit raw; symbol slots emit a relocation with the
        // Mach-O symbol prefix.
        assert!(asm.contains("    .quad 42\n"), "tag slot:\n{}", asm);
        assert!(
            asm.contains("    .quad _afs_modproc_m_method\n"),
            "symbol slot needs the _ prefix on Mach-O:\n{}",
            asm
        );
        assert_eq!(asm.matches(".quad").count(), 3, "three slots:\n{}", asm);
    }

    #[test]
    fn emit_vtable_quad_table_elf_symbols_unprefixed() {
        let asm = crate::codegen::shared::emit_globals(
            &[Global {
                name: "afs_vtable_m_t".into(),
                ty: IrType::Array(Box::new(IrType::Int(IntWidth::I64)), 3),
                initializer: Some(GlobalInit::QuadTable(vec![
                    QuadSlot::Int(7),
                    QuadSlot::Sym("afs_vtable_m_base".into()),
                    QuadSlot::Sym("afs_modproc_m_method".into()),
                ])),
            }],
            &crate::target::TargetLayout::LP64,
            crate::codegen::shared::GlobalsDialect::Elf,
        );
        assert!(
            asm.contains(".globl afs_vtable_m_t"),
            "ELF vtable external linkage:\n{}",
            asm
        );
        // ELF carries no symbol prefix: parent pointer and method slot
        // are plain symbol names.
        assert!(
            asm.contains("    .quad afs_vtable_m_base\n"),
            "parent pointer slot:\n{}",
            asm
        );
        assert!(
            asm.contains("    .quad afs_modproc_m_method\n"),
            "method slot unprefixed on ELF:\n{}",
            asm
        );
    }

    #[test]
    fn emit_byte_array_global_uses_natural_alignment() {
        let asm = emit_globals_lp64_test(&[Global {
            name: "history".into(),
            ty: IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 400),
            initializer: Some(GlobalInit::Zero),
        }]);

        assert!(
            asm.contains(".zerofill __DATA,__bss,_history,400,4"),
            "byte-array globals should reserve zero-fill storage with 16-byte alignment:\n{}",
            asm
        );
    }

    #[test]
    fn emit_nested_byte_array_global_uses_full_storage_size() {
        let asm = emit_globals_lp64_test(&[Global {
            name: "command_cache".into(),
            ty: IrType::Array(
                Box::new(IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 264)),
                4,
            ),
            initializer: Some(GlobalInit::Zero),
        }]);

        assert!(
            asm.contains(".zerofill __DATA,__bss,_command_cache,1056,4"),
            "nested byte-array globals should reserve their full zero-fill size:\n{}",
            asm
        );
    }

    #[test]
    fn emit_mov_reg_truncates_x_source_through_w_view() {
        let mf = MachineFunction::new("test".into());
        let inst = MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp32(21)),
                MachineOperand::PhysReg(PhysReg::Gp(20)),
            ],
            def: None,
        };

        assert_eq!(emit_inst(&inst, &mf), "mov w21, w20");
    }

    #[test]
    fn emit_fcvt_uses_fp_register_widths() {
        let mf = MachineFunction::new("test".into());
        let to_single = MachineInst {
            opcode: ArmOpcode::FcvtSD,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Fp(0)),
                MachineOperand::PhysReg(PhysReg::Fp(1)),
            ],
            def: None,
        };
        let to_double = MachineInst {
            opcode: ArmOpcode::FcvtDS,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Fp32(2)),
                MachineOperand::PhysReg(PhysReg::Fp32(3)),
            ],
            def: None,
        };

        assert_eq!(emit_inst(&to_single, &mf), "fcvt s0, d1");
        assert_eq!(emit_inst(&to_double, &mf), "fcvt d2, s3");
    }

    #[test]
    fn emit_large_negative_pair_offsets_use_scratch_addressing() {
        let mf = MachineFunction::new("test".into());
        let stp = MachineInst {
            opcode: ArmOpcode::StpOffset,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(-544),
            ],
            def: None,
        };
        let ldp = MachineInst {
            opcode: ArmOpcode::LdpOffset,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(2)),
                MachineOperand::PhysReg(PhysReg::Gp(3)),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(-544),
            ],
            def: None,
        };

        let stp_asm = emit_inst(&stp, &mf);
        let ldp_asm = emit_inst(&ldp, &mf);
        assert!(
            stp_asm.contains("sub x9, x29, #544"),
            "large negative stp offset should synthesize address: {}",
            stp_asm
        );
        assert!(
            ldp_asm.contains("sub x9, x29, #544"),
            "large negative ldp offset should synthesize address: {}",
            ldp_asm
        );
        assert!(
            !stp_asm.contains("[x29, #-544]"),
            "stp should not emit out-of-range raw offset: {}",
            stp_asm
        );
        assert!(
            !ldp_asm.contains("[x29, #-544]"),
            "ldp should not emit out-of-range raw offset: {}",
            ldp_asm
        );
    }

    #[test]
    fn emit_internal_only_function_as_private_extern() {
        let mut mf = MachineFunction::new("helper".into());
        mf.internal_only = true;

        let asm = emit_function(&mf);

        assert!(
            asm.contains(".private_extern _helper"),
            "internal-only functions should not be emitted as globals:\n{}",
            asm
        );
        assert!(
            !asm.contains(".globl _helper"),
            "internal-only functions should not keep external linkage:\n{}",
            asm
        );
    }

    // ---- NEON SIMD emit smoke tests (Sprint 12 Stage 2) ----
    //
    // The vectorizer doesn't generate any of these yet, but the emit
    // formatters can be exercised directly by hand-building a
    // MachineInst and feeding it through `emit_inst`. These tests
    // pin the assembly text form so future codegen wiring has a
    // golden reference.

    use crate::codegen::mir::{ArmOpcode, MachineFunction, MachineInst, MachineOperand, RegClass};

    fn emit_one(opcode: ArmOpcode, operands: Vec<MachineOperand>) -> String {
        let mut mf = MachineFunction::new("t".into());
        mf.new_block("entry");
        let inst = MachineInst {
            opcode,
            operands,
            def: None,
        };
        emit_inst(&inst, &mf)
    }

    #[test]
    fn emit_fadd_v_4s_form() {
        let mut mf = MachineFunction::new("t".into());
        let v0 = mf.new_vreg(RegClass::V128);
        let v1 = mf.new_vreg(RegClass::V128);
        let v2 = mf.new_vreg(RegClass::V128);
        let asm = emit_one(
            ArmOpcode::FaddV4S,
            vec![
                MachineOperand::VReg(v0),
                MachineOperand::VReg(v1),
                MachineOperand::VReg(v2),
            ],
        );
        let _ = mf;
        // afs-as dialect: shape suffix on mnemonic, bare regs.
        assert_eq!(asm, "fadd.4s v0, v1, v2");
    }

    #[test]
    fn emit_fadd_v_2d_form() {
        let asm = emit_one(
            ArmOpcode::FaddV2D,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(1)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(2)),
            ],
        );
        assert_eq!(asm, "fadd.2d v0, v1, v2");
    }

    #[test]
    fn emit_fmla_v_4s_form() {
        let asm = emit_one(
            ArmOpcode::FmlaV4S,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(1)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(2)),
            ],
        );
        assert_eq!(asm, "fmla.4s v0, v1, v2");
    }

    #[test]
    fn emit_addv_4s_reduction_form() {
        let asm = emit_one(
            ArmOpcode::Addv4S,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(1)),
            ],
        );
        assert_eq!(asm, "addv.4s s0, v1");
    }

    #[test]
    fn emit_dup_gen_4s_broadcasts_w_register() {
        let asm = emit_one(
            ArmOpcode::DupGen4S,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::PhysReg(crate::codegen::mir::PhysReg::Gp32(2)),
            ],
        );
        assert_eq!(asm, "dup.4s v0, w2");
    }

    #[test]
    fn emit_dup_el_4s_broadcasts_fp_lane_zero() {
        // Splatting an Fp32 scalar (which lives in v2's lane 0) into
        // a 4×f32 vector uses the lane-dup form. The gp form
        // `dup.4s v0, s2` is rejected by the assembler. afs-as
        // dialect: bare `vN[L]` (no `.s` suffix), with the lane
        // element width encoded into the `dup.4s` mnemonic.
        let asm = emit_one(
            ArmOpcode::DupEl4S,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(2)),
            ],
        );
        assert_eq!(asm, "dup.4s v0, v2[0]");
    }

    #[test]
    fn emit_dup_el_2d_broadcasts_fp_lane_zero() {
        let asm = emit_one(
            ArmOpcode::DupEl2D,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(2)),
            ],
        );
        assert_eq!(asm, "dup.2d v0, v2[0]");
    }

    #[test]
    fn emit_ldr_q_form() {
        let asm = emit_one(
            ArmOpcode::LdrQ,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::PhysReg(crate::codegen::mir::PhysReg::Gp(1)),
                MachineOperand::Imm(16),
            ],
        );
        assert_eq!(asm, "ldr q0, [x1, #16]");
    }

    #[test]
    fn emit_str_q_form() {
        let asm = emit_one(
            ArmOpcode::StrQ,
            vec![
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::PhysReg(crate::codegen::mir::PhysReg::Gp(1)),
                MachineOperand::Imm(0),
            ],
        );
        assert_eq!(asm, "str q0, [x1, #0]");
    }

    #[test]
    fn emit_umov_extracts_lane() {
        let asm = emit_one(
            ArmOpcode::Umov4S,
            vec![
                MachineOperand::PhysReg(crate::codegen::mir::PhysReg::Gp32(3)),
                MachineOperand::VReg(crate::codegen::mir::VRegId(0)),
                MachineOperand::Imm(2),
            ],
        );
        assert_eq!(asm, "umov.s w3, v0[2]");
    }
}
