//! Assembly text emission — converts Machine IR to ARM64 assembly text.
//!
//! Produces output compatible with both afs-as and Apple's system assembler.

use std::fmt::Write;
use super::mir::*;

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

/// Emit a single machine instruction as assembly text.
fn emit_inst(inst: &MachineInst, mf: &MachineFunction) -> String {
    match inst.opcode {
        ArmOpcode::AddReg => format!("add {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::AddImm => {
            let dest = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            let offset = match &inst.operands[2] {
                MachineOperand::FrameSlot(off) => format!("#{}", off),
                MachineOperand::Imm(-1) => {
                    // Sentinel: prologue FP setup → frame_size - 16
                    format!("#{}", mf.frame.size.saturating_sub(16))
                }
                MachineOperand::Imm(v) => format!("#{}", v),
                _ => op_str(&inst.operands[2]),
            };
            format!("add {}, {}, {}", dest, base, offset)
        }
        ArmOpcode::SubReg => format!("sub {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),
        ArmOpcode::SubImm => {
            let imm = match &inst.operands[2] {
                MachineOperand::Imm(-1) => {
                    // Sentinel: epilogue SP restore → frame_size - 16
                    mf.frame.size.saturating_sub(16) as i64
                }
                MachineOperand::Imm(v) => *v,
                _ => 0,
            };
            format!("sub {}, {}, #{}", op_str(&inst.operands[0]), op_str(&inst.operands[1]), imm)
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
        ArmOpcode::AsrReg => format!("asr {}, {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1]), op_str(&inst.operands[2])),

        ArmOpcode::CmpReg => format!("cmp {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),
        ArmOpcode::CmpImm => format!("cmp {}, #{}",
            op_str(&inst.operands[0]),
            if let MachineOperand::Imm(v) = &inst.operands[1] { *v } else { 0 }),
        ArmOpcode::Cset | ArmOpcode::FCset => {
            let cond = if let MachineOperand::Cond(c) = &inst.operands[1] { cond_str(*c) } else { "eq" };
            format!("cset {}, {}", op_str(&inst.operands[0]), cond)
        }
        ArmOpcode::FCmpReg => format!("fcmp {}, {}",
            op_str(&inst.operands[0]), op_str(&inst.operands[1])),

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

        ArmOpcode::LdrImm | ArmOpcode::LdrFpImm => {
            let dest = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            let offset_val = match &inst.operands[2] {
                MachineOperand::FrameSlot(off) => *off as i64,
                MachineOperand::Imm(v) => *v,
                _ => 0,
            };
            if (-256..=255).contains(&offset_val) {
                format!("ldr {}, [{}, #{}]", dest, base, offset_val)
            } else {
                // Large offset: compute address in x8, then load.
                format!("mov x8, #{}\n    add x8, {}, x8\n    ldr {}, [x8]",
                    offset_val, base, dest)
            }
        }
        ArmOpcode::StrImm | ArmOpcode::StrFpImm => {
            let src = op_str(&inst.operands[0]);
            let base = op_str(&inst.operands[1]);
            let offset_val = match &inst.operands[2] {
                MachineOperand::FrameSlot(off) => *off as i64,
                MachineOperand::Imm(v) => *v,
                _ => 0,
            };
            if (-256..=255).contains(&offset_val) {
                format!("str {}, [{}, #{}]", src, base, offset_val)
            } else {
                // Large offset: compute address in x8, then store.
                format!("mov x8, #{}\n    add x8, {}, x8\n    str {}, [x8]",
                    offset_val, base, src)
            }
        }

        ArmOpcode::StpPre => {
            let frame_size = mf.frame.size;
            let stp_offset = frame_size - 16;
            if stp_offset <= 504 {
                format!("sub sp, sp, #{}\n    stp x29, x30, [sp, #{}]",
                    frame_size, stp_offset)
            } else {
                // Large frame: use SUB + STP at [sp] approach.
                format!("sub sp, sp, #{}\n    str x29, [sp, #{}]\n    str x30, [sp, #{}]",
                    frame_size, stp_offset, stp_offset + 8)
            }
        }
        ArmOpcode::LdpPost => {
            let frame_size = mf.frame.size;
            let ldp_offset = frame_size - 16;
            if ldp_offset <= 504 {
                format!("ldp x29, x30, [sp, #{}]\n    add sp, sp, #{}",
                    ldp_offset, frame_size)
            } else {
                format!("ldr x29, [sp, #{}]\n    ldr x30, [sp, #{}]\n    add sp, sp, #{}",
                    ldp_offset, ldp_offset + 8, frame_size)
            }
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
            if let MachineOperand::ConstPool(idx) = &inst.operands[1] {
                let label = const_pool_label(&mf.name, *idx);
                format!("adrp {0}, {1}@PAGE\n    add {0}, {0}, {1}@PAGEOFF",
                    op_str(&inst.operands[0]), label)
            } else {
                "nop ; bad adrp+add".into()
            }
        }

        ArmOpcode::B => {
            if let MachineOperand::BlockRef(id) = &inst.operands[0] {
                format!("b {}", mf.block(*id).label)
            } else { "b ???".into() }
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
