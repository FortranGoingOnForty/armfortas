//! AT&T/ELF assembly printer for the x86_64 backend (sprint x03).
//!
//! AT&T rules throughout: `%`-prefixed registers, `$`-immediates,
//! source before destination, explicit size suffixes on every mnemonic
//! (gas would infer sizes from register operands, but memory/immediate
//! forms are ambiguous and its fallback behavior is a trap).
//!
//! ELF conventions: no underscore prefix on symbols, `.L`-prefixed
//! local labels (bare `L` becomes a real ELF symbol), `.p2align` only
//! (`.align` means bytes on x86 gas), `.type`/`.size` on every
//! function and object, constant pools in `.rodata`.

use super::mir::{OpSize, X86Function, X86Inst, X86Opcode, X86Operand, X86Reg};
use std::fmt::Write;

/// ELF symbol naming is identity — centralized anyway so the Mach-O
/// underscore convention has an explicit, greppable counterpart.
pub fn symbol(name: &str) -> String {
    name.to_string()
}

/// Local (non-symtab) label for a block.
fn block_label(func: &X86Function, block_idx: usize) -> String {
    format!(".L{}_{}", func.name, func.blocks[block_idx].id.0)
}

fn block_label_by_id(func: &X86Function, id: super::mir::MBlockId) -> String {
    format!(".L{}_{}", func.name, id.0)
}

/// Print one function as AT&T/ELF text.
pub fn emit_function(func: &X86Function) -> String {
    let mut out = String::new();
    let name = symbol(&func.name);
    writeln!(out, ".text").unwrap();
    writeln!(out, ".globl {}", name).unwrap();
    writeln!(out, ".p2align 4").unwrap();
    writeln!(out, ".type {},@function", name).unwrap();
    writeln!(out, "{}:", name).unwrap();
    for (idx, block) in func.blocks.iter().enumerate() {
        if idx != 0 {
            writeln!(out, "{}:", block_label(func, idx)).unwrap();
        }
        for inst in &block.insts {
            writeln!(out, "    {}", emit_inst(inst, func)).unwrap();
        }
    }
    writeln!(out, ".size {}, .-{}", name, name).unwrap();
    out
}

/// `.rodata` f64 constant (8-byte aligned). Constant pools are
/// read-only on ELF — not the writable `__DATA,__const` section the
/// ARM/Mach-O emitter uses.
pub fn emit_rodata_f64(label: &str, value: f64) -> String {
    let mut out = String::new();
    writeln!(out, ".section .rodata").unwrap();
    writeln!(out, ".p2align 3").unwrap();
    writeln!(out, "{}:", label).unwrap();
    writeln!(out, "    .quad 0x{:016x}", value.to_bits()).unwrap();
    out
}

/// `.data` 8-byte integer global with `.type`/`.size`.
pub fn emit_data_quad(name: &str, value: i64, hidden: bool) -> String {
    let mut out = String::new();
    let sym = symbol(name);
    writeln!(out, ".data").unwrap();
    writeln!(out, ".globl {}", sym).unwrap();
    if hidden {
        // ELF analog of Mach-O `.private_extern`.
        writeln!(out, ".hidden {}", sym).unwrap();
    }
    writeln!(out, ".p2align 3").unwrap();
    writeln!(out, ".type {},@object", sym).unwrap();
    writeln!(out, ".size {}, 8", sym).unwrap();
    writeln!(out, "{}:", sym).unwrap();
    writeln!(out, "    .quad {}", value).unwrap();
    out
}

fn operand(op: &X86Operand, size: OpSize) -> String {
    match op {
        X86Operand::Reg(r) => r.name(size).to_string(),
        X86Operand::Imm(v) => format!("${}", v),
        X86Operand::Mem {
            base,
            index,
            scale,
            disp,
        } => {
            let mut s = String::new();
            if *disp != 0 || (base.is_none() && index.is_none()) {
                write!(s, "{}", disp).unwrap();
            }
            s.push('(');
            if let Some(b) = base {
                s.push_str(b.name(OpSize::Q));
            }
            if let Some(i) = index {
                write!(s, ",{},{}", i.name(OpSize::Q), scale).unwrap();
            }
            s.push(')');
            s
        }
        X86Operand::RipLabel(l) => format!("{}(%rip)", l),
        X86Operand::Extern(name) => symbol(name),
        X86Operand::VReg(v) => panic!(
            "unallocated vreg {:?} reached the printer — regalloc (x05) must run first",
            v
        ),
        X86Operand::FrameSlot(s) => panic!(
            "unresolved frame slot {} reached the printer — frame layout (x05) must run first",
            s
        ),
        X86Operand::Cond(_) | X86Operand::BlockRef(_) => {
            panic!("condition/block operands are printed by their opcodes, not as values")
        }
    }
}

/// The tie assertion. Printing `d = add a, b` as `addq b, d` computes
/// `d = d + b`, which is only `a + b` when `d == a`. Before x05 no pass
/// guarantees ties, so the printer checks instead of trusting — a
/// violated tie must never print as silently wrong two-operand asm.
fn assert_tie(inst: &X86Inst) {
    if let Some(idx) = inst.opcode.tied_use() {
        let def = inst
            .def
            .as_ref()
            .unwrap_or_else(|| panic!("{:?} is destructive but has no def", inst.opcode));
        let tied = inst.operands.get(idx).unwrap_or_else(|| {
            panic!(
                "{:?} ties operand {} but the instruction has {} operands",
                inst.opcode,
                idx,
                inst.operands.len()
            )
        });
        assert_eq!(
            def, tied,
            "violated tie: {:?} requires def == operand {} (got {:?} vs {:?})",
            inst.opcode, idx, def, tied
        );
    }
}

fn emit_inst(inst: &X86Inst, func: &X86Function) -> String {
    use X86Opcode::*;
    assert_tie(inst);
    let s = inst.size;
    let suf = s.suffix();
    let def = || operand(inst.def.as_ref().expect("missing def"), s);
    let op = |i: usize| operand(&inst.operands[i], s);
    let opq = |i: usize| operand(&inst.operands[i], OpSize::Q);
    let cond = |i: usize| match &inst.operands[i] {
        X86Operand::Cond(c) => *c,
        other => panic!("expected condition operand, got {:?}", other),
    };
    let block = |i: usize| match &inst.operands[i] {
        X86Operand::BlockRef(id) => block_label_by_id(func, *id),
        other => panic!("expected block operand, got {:?}", other),
    };

    match inst.opcode {
        MovRR | MovRM => format!("mov{} {}, {}", suf, op(0), def()),
        MovRI => format!("mov{} {}, {}", suf, op(0), def()),
        MovMR | MovMI => format!("mov{} {}, {}", suf, op(0), op(1)),
        Movsx { src } => {
            // movs{src}{dst}: e.g. movslq %eax, %rax
            format!(
                "movs{}{} {}, {}",
                src.suffix(),
                suf,
                operand(&inst.operands[0], src),
                def()
            )
        }
        Movzx { src } => {
            // 32→64 zero-extension is plain movl (implicit zeroing of the
            // upper half); movzlq does not exist.
            assert!(
                !(src == OpSize::L && s == OpSize::Q),
                "32->64 zero extension is movl to the 32-bit register, not movzlq"
            );
            format!(
                "movz{}{} {}, {}",
                src.suffix(),
                suf,
                operand(&inst.operands[0], src),
                def()
            )
        }
        Lea => format!("lea{} {}, {}", suf, op(0), def()),
        Add => format!("add{} {}, {}", suf, op(1), def()),
        Sub => format!("sub{} {}, {}", suf, op(1), def()),
        Imul => format!("imul{} {}, {}", suf, op(1), def()),
        And => format!("and{} {}, {}", suf, op(1), def()),
        Or => format!("or{} {}, {}", suf, op(1), def()),
        Xor => format!("xor{} {}, {}", suf, op(1), def()),
        Neg => format!("neg{} {}", suf, def()),
        Not => format!("not{} {}", suf, def()),
        Shl | Shr | Sar => {
            let mn = match inst.opcode {
                Shl => "shl",
                Shr => "shr",
                _ => "sar",
            };
            let count = match &inst.operands[1] {
                X86Operand::Imm(v) => format!("${}", v),
                X86Operand::Reg(X86Reg::Rcx) => "%cl".to_string(),
                other => panic!("shift count must be an immediate or %cl, got {:?}", other),
            };
            format!("{}{} {}, {}", mn, suf, count, def())
        }
        Cltd => "cltd".to_string(),
        Cqto => "cqto".to_string(),
        Idiv => format!("idiv{} {}", suf, op(0)),
        // Flags from `lhs - rhs`: `cmpq %rbx, %rax; jl` branches when
        // %rax < %rbx. MIR order is Cmp(lhs, rhs); AT&T prints rhs
        // first. The printer tests pin this — get it backwards and
        // loops run zero or infinite times.
        Cmp => format!("cmp{} {}, {}", suf, op(1), op(0)),
        Test => format!("test{} {}, {}", suf, op(1), op(0)),
        Setcc => format!(
            "set{} {}",
            cond(0).mnemonic(),
            operand(inst.def.as_ref().expect("setcc def"), OpSize::B)
        ),
        Jmp => format!("jmp {}", block(0)),
        Jcc => format!("j{} {}", cond(0).mnemonic(), block(1)),
        // Bare name for now; @PLT decoration is link-policy (x06).
        Call => format!("call {}", opq(0)),
        Ret => "ret".to_string(),
        Push => format!("pushq {}", opq(0)),
        Pop => format!("popq {}", opq(0)),
        Movss => format!("movss {}, {}", op(0), def()),
        Movsd => format!("movsd {}, {}", op(0), def()),
        Addss | Addsd | Subss | Subsd | Mulss | Mulsd | Divss | Divsd | Xorps | Xorpd | Andps
        | Andpd => {
            let mn = match inst.opcode {
                Addss => "addss",
                Addsd => "addsd",
                Subss => "subss",
                Subsd => "subsd",
                Mulss => "mulss",
                Mulsd => "mulsd",
                Divss => "divss",
                Divsd => "divsd",
                Xorps => "xorps",
                Xorpd => "xorpd",
                Andps => "andps",
                _ => "andpd",
            };
            format!("{} {}, {}", mn, op(1), def())
        }
        // Non-destructive two-operand form: `sqrtsd %src, %dst`.
        Sqrtss => format!("sqrtss {}, {}", op(0), def()),
        Sqrtsd => format!("sqrtsd {}, {}", op(0), def()),
        Ucomiss => format!("ucomiss {}, {}", op(1), op(0)),
        Ucomisd => format!("ucomisd {}, {}", op(1), op(0)),
        // The integer side of cvt carries the suffix (cvtsi2sdq for a
        // 64-bit source); the FP side is pinned by the mnemonic.
        Cvtsi2ss => format!("cvtsi2ss{} {}, {}", suf, op(0), def()),
        Cvtsi2sd => format!("cvtsi2sd{} {}, {}", suf, op(0), def()),
        Cvttss2si => format!("cvttss2si{} {}, {}", suf, op(0), def()),
        Cvttsd2si => format!("cvttsd2si{} {}, {}", suf, op(0), def()),
        Cvtss2sd => format!("cvtss2sd {}, {}", op(0), def()),
        Cvtsd2ss => format!("cvtsd2ss {}, {}", op(0), def()),
    }
}
