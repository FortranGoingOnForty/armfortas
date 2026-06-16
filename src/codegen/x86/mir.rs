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
    /// Scalar f32/f64 in the low lane of an XMM register.
    Xmm,
    /// Full 128-bit XMM vector value (x10). Same physical registers
    /// as `Xmm`; the class drives slot width (16 bytes) and spill
    /// traffic (movups) in the allocator.
    Xmm128,
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
        // One table: (q, l, w, b). Low-byte forms only — never %ah..%dh.
        const NAMES: [(&str, &str, &str, &str); 16] = [
            ("%rax", "%eax", "%ax", "%al"),
            ("%rcx", "%ecx", "%cx", "%cl"),
            ("%rdx", "%edx", "%dx", "%dl"),
            ("%rbx", "%ebx", "%bx", "%bl"),
            ("%rsp", "%esp", "%sp", "%spl"),
            ("%rbp", "%ebp", "%bp", "%bpl"),
            ("%rsi", "%esi", "%si", "%sil"),
            ("%rdi", "%edi", "%di", "%dil"),
            ("%r8", "%r8d", "%r8w", "%r8b"),
            ("%r9", "%r9d", "%r9w", "%r9b"),
            ("%r10", "%r10d", "%r10w", "%r10b"),
            ("%r11", "%r11d", "%r11w", "%r11b"),
            ("%r12", "%r12d", "%r12w", "%r12b"),
            ("%r13", "%r13d", "%r13w", "%r13b"),
            ("%r14", "%r14d", "%r14w", "%r14b"),
            ("%r15", "%r15d", "%r15w", "%r15b"),
        ];
        let (q, l, w, b) = NAMES[self as usize];
        match size {
            OpSize::Q => q,
            OpSize::L => l,
            OpSize::W => w,
            OpSize::B => b,
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
    /// reg <- mem. Address forms: a `Mem{..}` operand, a `FrameSlot`
    /// (resolved by frame layout), or — pre-regalloc only — a single
    /// `VReg` of class Gp64 meaning "the address lives in this vreg";
    /// regalloc rewrites that form to `Mem { base: assigned_reg, .. }`.
    /// `MovMR` and the scalar SSE moves use the same convention.
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
    /// Sign-extending LOAD: operand 0 is an ADDRESS (the MovRM
    /// convention), so regalloc dereferences through it. Same
    /// mnemonic as Movsx; split because a plain Movsx operand is a
    /// VALUE register and the allocator cannot tell pointers from
    /// values in a vreg (x07: `if (logical_dummy)` branched on the
    /// low byte of the pointer).
    MovsxRM {
        src: OpSize,
    },
    /// Zero-extending LOAD: operand 0 is an ADDRESS. See MovsxRM.
    MovzxRM {
        src: OpSize,
    },
    Lea,
    /// Indirect call through a register: `call *%reg`.
    CallReg,
    /// `movq %r64, %xmm` — raw 8-byte bits GP→XMM (SSE2). Carries
    /// complex(4) values (packed re/im) across the psABI's SSE-class
    /// boundary without touching memory.
    MovqGpToXmm,
    /// `movq %xmm, %r64` — the reverse.
    MovqXmmToGp,

    // ---- Integer arithmetic (destructive: tied to operand 0) ----
    Add,
    Sub,
    /// Add-with-carry / subtract-with-borrow: the i128 pair ops. The
    /// flags producer (Add/Sub on the low quad) must be ADJACENT —
    /// the naive allocator only inserts mov-family spill traffic,
    /// which preserves RFLAGS (x05 module doc), and the i128 emitters
    /// use physical registers + frame slots so no spill lands between
    /// the pair at all.
    Adc,
    Sbb,
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
    /// Bit scan reverse/forward. Used to synthesize CLZ/CTZ with an
    /// explicit zero branch, avoiding BMI LZCNT/TZCNT.
    Bsr,
    Bsf,

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
    Andps,
    Andpd,
    Sqrtss,
    Sqrtsd,

    // ---- Packed SSE2 (x10 vectorizer; nothing above SSE2, ever —
    // the CI ISA-ceiling grep enforces it) ----
    /// Unaligned 128-bit load/store/move. Used for every vector
    /// load/store regardless of element type: descriptors don't
    /// guarantee 16-byte alignment and movups on aligned data costs
    /// the same on every µarch that matters.
    Movups,
    /// Register-register 128-bit move.
    Movaps,
    Addps,
    Addpd,
    Subps,
    Subpd,
    Mulps,
    Mulpd,
    Divps,
    Divpd,
    Sqrtps,
    Sqrtpd,
    /// NaN/zero semantics: returns the SECOND operand (the AT&T src)
    /// when either input is NaN. Isel orders operands so this matches
    /// the scalar select-of-compare lowering.
    Minps,
    Minpd,
    Maxps,
    Maxpd,
    /// Packed integer add/sub, 4×i32 / 2×i64 (tied).
    Paddd,
    Paddq,
    Psubd,
    Psubq,
    /// Packed unsigned 32×32→64 multiply of the even lanes (tied):
    /// `pmuludq` multiplies lanes 0 and 2 of each operand into the two
    /// 64-bit result lanes. The building block for v4i32 multiply at
    /// SSE2 (no pmulld until SSE4.1) — two pmuludq over even/odd lanes
    /// plus a shuffle merge (x10c-3).
    Pmuludq,
    /// Packed bitwise (tied): mask select via pand/pandn/por.
    Pand,
    Pandn,
    Por,
    Pxor,
    /// Packed signed i32 compare-greater (tied); the only packed
    /// integer compare SSE2 offers besides pcmpeqd.
    Pcmpgtd,
    Pcmpeqd,
    /// Packed float compares: predicate immediate in operands
    /// (0=eq, 1=lt, 2=le, 4=ne, 5=nlt/ge, 6=nle/gt — isel uses
    /// only 0/1/2/4 plus operand swap).
    Cmpps,
    Cmppd,
    /// Lane shuffles for broadcast and reduction trees.
    Pshufd,
    Shufps,
    Movhlps,
    Unpcklpd,
    Punpcklqdq,
    /// GP <-> XMM lane 0 moves for broadcasts and extracts
    /// (movd/movq by size).
    Movd,
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
            Add | Sub | Adc | Sbb | Imul | And | Or | Xor | Neg | Not | Shl | Shr | Sar | Addss
            | Addsd | Subss | Subsd | Mulss | Mulsd | Divss | Divsd | Xorps | Xorpd | Andps
            | Andpd => Some(0),
            // Packed SSE2 is destructive two-operand like the scalar
            // SSE family. pshufd is NOT tied (imm + src → dst);
            // sqrtps/pd likewise (src → dst).
            Addps | Addpd | Subps | Subpd | Mulps | Mulpd | Divps | Divpd | Minps | Minpd
            | Maxps | Maxpd | Paddd | Paddq | Psubd | Psubq | Pmuludq | Pand | Pandn | Por
            | Pxor | Pcmpgtd | Pcmpeqd | Cmpps | Cmppd | Shufps | Movhlps | Unpcklpd
            | Punpcklqdq => Some(0),
            // Sqrtss/Sqrtsd are two-operand non-destructive
            // (`sqrtsd %src, %dst`) — deliberately not tied.
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

/// A stack-slot request from instruction selection. `FrameSlot(id)`
/// operands index this table; frame layout (x05 regalloc) assigns the
/// rbp-relative displacement. Mirrors the shape of ARM's
/// `StackFrame`/`FrameSlot` minus the eager offset assignment — a
/// disp32 reaches any offset, so layout can run after spill slots are
/// known.
#[derive(Debug, Clone)]
pub struct X86FrameSlot {
    pub id: i32,
    pub size: u64,
    pub align: u64,
}

/// A machine function.
#[derive(Debug, Clone)]
pub struct X86Function {
    /// Symbol name, unmangled: ELF symbols carry no underscore prefix.
    pub name: String,
    pub blocks: Vec<X86Block>,
    /// Per-function `.rodata` constants as (label, raw bits): FP
    /// immediates and the sign/abs masks isel references through
    /// `RipLabel`. The x86 analog of ARM's `const_pool`; bits-only
    /// because everything x05 selects fits one `.quad` (f32 bits sit
    /// in the low four bytes — `movss` reads only those).
    pub rodata: Vec<(String, u64)>,
    /// Frame slots requested by isel (allocas).
    /// Byte-blob constants (string literals): label → contents.
    pub rodata_bytes: Vec<(String, Vec<u8>)>,
    pub frame_slots: Vec<X86FrameSlot>,
    /// Largest rsp-relative outgoing stack-argument area any call site
    /// in this function needs.
    pub outgoing_arg_bytes: i64,
    /// Total prologue rsp subtraction (locals + outgoing, 16-aligned).
    /// Set by frame layout in regalloc (x05); 0 until then.
    pub frame_bytes: i64,
    next_vreg: u32,
    next_block: u32,
}

impl X86Function {
    pub fn new(name: String) -> Self {
        Self {
            name,
            blocks: vec![X86Block {
                id: MBlockId(0),
                insts: Vec::new(),
            }],
            rodata: Vec::new(),
            rodata_bytes: Vec::new(),
            frame_slots: Vec::new(),
            outgoing_arg_bytes: 0,
            frame_bytes: 0,
            next_vreg: 0,
            next_block: 1,
        }
    }

    /// Allocate a fresh virtual register.
    pub fn new_vreg(&mut self, class: X86RegClass) -> X86VReg {
        let vreg = X86VReg {
            id: VRegId(self.next_vreg),
            class,
        };
        self.next_vreg += 1;
        vreg
    }

    /// Create a new machine block.
    pub fn new_block(&mut self) -> MBlockId {
        let id = MBlockId(self.next_block);
        self.next_block += 1;
        self.blocks.push(X86Block {
            id,
            insts: Vec::new(),
        });
        id
    }

    /// Get a mutable block by ID.
    pub fn block_mut(&mut self, id: MBlockId) -> &mut X86Block {
        self.blocks
            .iter_mut()
            .find(|b| b.id == id)
            .expect("machine block not found")
    }

    /// Add (or reuse) a `.rodata` quad, returning its rip label.
    /// Dedup by bits is value-correct for both `movss` (reads the low
    /// four bytes) and `movsd` (reads all eight) consumers.
    pub fn add_rodata(&mut self, bits: u64) -> String {
        if let Some((label, _)) = self.rodata.iter().find(|(_, b)| *b == bits) {
            return label.clone();
        }
        let label = format!(".LC{}_{}", self.name, self.rodata.len());
        self.rodata.push((label.clone(), bits));
        label
    }

    /// Intern a byte blob in `.rodata`; duplicate contents share one
    /// label (same policy as `add_rodata`).
    pub fn add_rodata_bytes(&mut self, bytes: &[u8]) -> String {
        if let Some((label, _)) = self.rodata_bytes.iter().find(|(_, b)| b == bytes) {
            return label.clone();
        }
        let label = format!(".Lstr{}_{}", self.name, self.rodata_bytes.len());
        self.rodata_bytes.push((label.clone(), bytes.to_vec()));
        label
    }

    /// Allocate a frame slot; the returned id goes in `FrameSlot`
    /// operands.
    pub fn alloc_frame_slot(&mut self, size: u64, align: u64) -> i32 {
        let id = self.frame_slots.len() as i32;
        self.frame_slots.push(X86FrameSlot { id, size, align });
        id
    }

    /// Reserve outgoing stack-argument space for calls made by this
    /// function.
    pub fn reserve_outgoing_args(&mut self, bytes: i64) {
        if bytes > self.outgoing_arg_bytes {
            self.outgoing_arg_bytes = bytes;
        }
    }
}
