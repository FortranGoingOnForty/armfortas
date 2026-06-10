//! x86_64 Machine IR (sprint x03).
//!
//! Vocabulary and printer-facing shapes only: instruction selection
//! arrives in x05, the SysV classifier in x04. MIR stays three-address
//! (`d = Add a, b`); the destructive-operand reality of x86 is opcode
//! metadata (`X86Opcode::tied_use`), resolved before register
//! allocation by rewriting to `mov d, a; d = op d, b`. The printer
//! asserts the tie holds and panics otherwise — a violated tie must
//! never print as silently wrong two-operand assembly.

pub use crate::codegen::shared::{MBlockId, VRegId};

/// Register classes. No flags class — RFLAGS is implicit; the
/// discipline for instructions that read/clobber it is defined in x05.
/// `Gp8` exists for `setcc` results and the `cl` shift count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum X86RegClass {
    Gp64,
    Gp32,
    Gp8,
    /// Scalar f32/f64 now; SSE vectors in x10.
    Xmm,
}

/// Physical registers. GP names for all three widths derive from one
/// table (`gp_name`); the high-byte forms (%ah..%dh) are deliberately
/// absent — they cannot be encoded alongside REX-requiring registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum X86Reg {
    Rax,
    Rcx,
    Rdx,
    Rbx,
    Rsp,
    Rbp,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
    Xmm0,
    Xmm1,
    Xmm2,
    Xmm3,
    Xmm4,
    Xmm5,
    Xmm6,
    Xmm7,
    Xmm8,
    Xmm9,
    Xmm10,
    Xmm11,
    Xmm12,
    Xmm13,
    Xmm14,
    Xmm15,
}

impl X86Reg {
    pub fn is_gp(self) -> bool {
        (self as u8) < (X86Reg::Xmm0 as u8)
    }

    /// AT&T name at the given operand size. Xmm registers have one
    /// spelling regardless of scalar width.
    pub fn name(self, size: OpSize) -> &'static str {
        use X86Reg::*;
        if !self.is_gp() {
            return match self {
                Xmm0 => "%xmm0",
                Xmm1 => "%xmm1",
                Xmm2 => "%xmm2",
                Xmm3 => "%xmm3",
                Xmm4 => "%xmm4",
                Xmm5 => "%xmm5",
                Xmm6 => "%xmm6",
                Xmm7 => "%xmm7",
                Xmm8 => "%xmm8",
                Xmm9 => "%xmm9",
                Xmm10 => "%xmm10",
                Xmm11 => "%xmm11",
                Xmm12 => "%xmm12",
                Xmm13 => "%xmm13",
                Xmm14 => "%xmm14",
                Xmm15 => "%xmm15",
                _ => unreachable!(),
            };
        }
        // One table: (q, l, b). Low-byte forms only — never %ah..%dh.
        const NAMES: [(&str, &str, &str); 16] = [
            ("%rax", "%eax", "%al"),
            ("%rcx", "%ecx", "%cl"),
            ("%rdx", "%edx", "%dl"),
            ("%rbx", "%ebx", "%bl"),
            ("%rsp", "%esp", "%spl"),
            ("%rbp", "%ebp", "%bpl"),
            ("%rsi", "%esi", "%sil"),
            ("%rdi", "%edi", "%dil"),
            ("%r8", "%r8d", "%r8b"),
            ("%r9", "%r9d", "%r9b"),
            ("%r10", "%r10d", "%r10b"),
            ("%r11", "%r11d", "%r11b"),
            ("%r12", "%r12d", "%r12b"),
            ("%r13", "%r13d", "%r13b"),
            ("%r14", "%r14d", "%r14b"),
            ("%r15", "%r15d", "%r15b"),
        ];
        let (q, l, b) = NAMES[self as usize];
        match size {
            OpSize::Q => q,
            OpSize::L => l,
            OpSize::B => b,
            // 16-bit forms exist (%ax, %r8w, ...) but nothing emits
            // them yet; add the column when an op needs it rather
            // than carrying untested spellings.
            OpSize::W => panic!("16-bit register names not wired yet: {:?}", self),
        }
    }
}

/// Operand size → AT&T mnemonic suffix. Suffixes are emitted
/// unconditionally: gas can infer size from register operands, but
/// memory/immediate forms are ambiguous and its fallback behavior is a
/// trap, not a convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpSize {
    B,
    W,
    L,
    Q,
}

impl OpSize {
    pub fn suffix(self) -> char {
        match self {
            OpSize::B => 'b',
            OpSize::W => 'w',
            OpSize::L => 'l',
            OpSize::Q => 'q',
        }
    }
}

/// Condition codes for `jcc` / `setcc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum X86Cond {
    E,
    Ne,
    L,
    Le,
    G,
    Ge,
    B,
    Be,
    A,
    Ae,
    S,
    Ns,
    P,
    Np,
}

impl X86Cond {
    pub fn mnemonic(self) -> &'static str {
        match self {
            X86Cond::E => "e",
            X86Cond::Ne => "ne",
            X86Cond::L => "l",
            X86Cond::Le => "le",
            X86Cond::G => "g",
            X86Cond::Ge => "ge",
            X86Cond::B => "b",
            X86Cond::Be => "be",
            X86Cond::A => "a",
            X86Cond::Ae => "ae",
            X86Cond::S => "s",
            X86Cond::Ns => "ns",
            X86Cond::P => "p",
            X86Cond::Np => "np",
        }
    }
}

/// Virtual register with class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86VReg {
    pub id: VRegId,
    pub class: X86RegClass,
}

/// Operands. `RipLabel` ships now even though nothing selects it —
/// retrofitting rip-relative addressing onto an absolute-only operand
/// set is the expensive path.
#[derive(Debug, Clone, PartialEq)]
pub enum X86Operand {
    VReg(X86VReg),
    Reg(X86Reg),
    Imm(i64),
    /// Frame-relative slot; resolved to disp(%rbp) by frame layout (x05).
    FrameSlot(i32),
    /// `disp(base,index,scale)`.
    Mem {
        base: Option<X86Reg>,
        index: Option<X86Reg>,
        scale: u8,
        disp: i64,
    },
    /// `sym(%rip)`.
    RipLabel(String),
    Cond(X86Cond),
    BlockRef(MBlockId),
    Extern(String),
}

/// Opcodes — the starter set x03 needs to print real functions; grown
/// in x05 as instruction selection needs ops. Operand size lives on the
/// instruction (`X86Inst::size`); fixed-size ops (Setcc, cvt forms)
/// ignore it where the mnemonic pins the width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum X86Opcode {
    // ---- Moves ----
    MovRR,
    MovRI,
    /// reg <- mem
    MovRM,
    /// mem <- reg
    MovMR,
    /// mem <- imm
    MovMI,
    /// Sign-extend (movsbq/movslq/... per src/dst sizes; x05 wires the
    /// width pair; x03 prints movs{src}{dst} from `size2`).
    Movsx {
        src: OpSize,
    },
    /// Zero-extend. NOTE: 32→64 zero extension is `movl` (implicit
    /// zeroing), not movzlq — x05 concern, printer rejects that pair.
    Movzx {
        src: OpSize,
    },
    Lea,

    // ---- Integer arithmetic (destructive: tied to operand 0) ----
    Add,
    Sub,
    Imul,
    And,
    Or,
    Xor,
    Neg,
    Not,
    /// Shift family: count is an Imm or %cl (x05 enforces).
    Shl,
    Shr,
    Sar,

    // ---- Division helpers ----
    /// Sign-extend %eax into %edx:%eax.
    Cltd,
    /// Sign-extend %rax into %rdx:%rax.
    Cqto,
    /// Signed divide of %rdx:%rax by the operand.
    Idiv,

    // ---- Compare / test / conditions ----
    Cmp,
    Test,
    Setcc,

    // ---- Control flow ----
    Jmp,
    Jcc,
    Call,
    Ret,

    // ---- Stack ----
    Push,
    Pop,

    // ---- Scalar SSE ----
    Movss,
    Movsd,
    Addss,
    Addsd,
    Subss,
    Subsd,
    Mulss,
    Mulsd,
    Divss,
    Divsd,
    Ucomiss,
    Ucomisd,
    Cvtsi2ss,
    Cvtsi2sd,
    Cvttss2si,
    Cvttsd2si,
    Cvtss2sd,
    Cvtsd2ss,
    Xorps,
    Xorpd,
}

impl X86Opcode {
    /// For destructive-operand instructions, the operand index the def
    /// is tied to. Ties are a property of the opcode — a per-inst field
    /// would be 99% redundant and could disagree with the opcode.
    /// (Mirrors LLVM's `Constraints = "$src1 = $dst"` metadata on the
    /// X86 arithmetic family.)
    pub fn tied_use(&self) -> Option<usize> {
        use X86Opcode::*;
        match self {
            Add | Sub | Imul | And | Or | Xor | Neg | Not | Shl | Shr | Sar | Addss | Addsd
            | Subss | Subsd | Mulss | Mulsd | Divss | Divsd | Xorps | Xorpd => Some(0),
            _ => None,
        }
    }
}

/// A machine instruction. `def` is the value the instruction writes
/// (register-allocated x05+; hand-built fixtures use physical regs).
#[derive(Debug, Clone)]
pub struct X86Inst {
    pub opcode: X86Opcode,
    pub size: OpSize,
    pub operands: Vec<X86Operand>,
    pub def: Option<X86Operand>,
}

/// A machine basic block. Labels print as `.L{fn}_{n}` — the `.L`
/// prefix keeps them out of the ELF symbol table (bare `L` is a Mach-O
/// convention that becomes a real symbol on ELF).
#[derive(Debug, Clone)]
pub struct X86Block {
    pub id: MBlockId,
    pub insts: Vec<X86Inst>,
}

/// A machine function.
#[derive(Debug, Clone)]
pub struct X86Function {
    /// Symbol name, unmangled: ELF symbols carry no underscore prefix.
    pub name: String,
    pub blocks: Vec<X86Block>,
}

impl X86Function {
    pub fn new(name: String) -> Self {
        Self {
            name,
            blocks: vec![X86Block {
                id: MBlockId(0),
                insts: Vec::new(),
            }],
        }
    }
}
