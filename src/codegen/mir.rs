//! Machine IR — low-level representation between SSA IR and ARM64 assembly.
//!
//! Uses virtual registers (VReg) that will be assigned to physical registers
//! by the register allocator. Before allocation, all vregs are spilled.

/// Virtual register identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VRegId(pub u32);

/// Virtual register with type class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VReg {
    pub id: VRegId,
    pub class: RegClass,
}

/// Register class — determines which physical registers can hold this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegClass {
    /// General purpose (X0-X28, excluding x18/x29/x30).
    Gp64,
    /// 32-bit general purpose (W0-W28).
    Gp32,
    /// FP/SIMD double (D0-D31).
    Fp64,
    /// FP/SIMD single (S0-S31).
    Fp32,
}

/// ARM64 opcodes that we emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmOpcode {
    // ---- Integer arithmetic ----
    AddReg,      // ADD Xd, Xn, Xm
    AddsReg,     // ADDS Xd, Xn, Xm (sets flags)
    AdcReg,      // ADC Xd, Xn, Xm
    AddImm,      // ADD Xd, Xn, #imm
    SubReg,      // SUB Xd, Xn, Xm
    SubsReg,     // SUBS Xd, Xn, Xm (sets flags)
    SbcReg,      // SBC Xd, Xn, Xm
    SubImm,      // SUB Xd, Xn, #imm
    Mul,         // MUL Xd, Xn, Xm
    Sdiv,        // SDIV Xd, Xn, Xm
    Msub,        // MSUB Xd, Xn, Xm, Xa  (for imod: a - (a/b)*b)
    Neg,         // NEG Xd, Xm  (alias: SUB Xd, XZR, Xm)

    // ---- Logic ----
    AndReg,
    OrrReg,
    EorReg,
    OrnReg,      // MVN is ORN Xd, XZR, Xm

    // ---- Shifts ----
    LslReg,
    LsrReg,
    AsrReg,

    // ---- Bit manipulation ----
    Mvn,         // MVN Xd, Xm  (bitwise NOT, alias: ORN Xd, XZR, Xm)
    Clz,         // CLZ Xd, Xn  (count leading zeros)
    Rbit,        // RBIT Xd, Xn (reverse bits)

    // ---- Comparison & select ----
    CmpReg,      // CMP Xn, Xm  (alias: SUBS XZR, Xn, Xm)
    CmpImm,      // CMP Xn, #imm
    Cset,        // CSET Xd, cond
    CselReg,     // CSEL Xd, Xn, Xm, cond
    FCmpReg,     // FCMP Dn, Dm
    FCset,       // CSET Xd, cond  (after FCMP)
    FcselReg,    // FCSEL Dd, Dn, Dm, cond

    // ---- Float arithmetic ----
    FaddS, FaddD,
    FsubS, FsubD,
    FmulS, FmulD,
    FdivS, FdivD,
    FnegS, FnegD,
    FabsS, FabsD,
    FsqrtS, FsqrtD,
    // Fused multiply-add/subtract (FMADD/FMSUB/FNMSUB).
    // 3-source: dest = Sa ± Sn*Sm.
    FmaddS, FmaddD,   // FMADD:  dest = Sa + Sn*Sm
    FmsubS, FmsubD,   // FMSUB:  dest = Sa - Sn*Sm
    FnmsubS, FnmsubD, // FNMSUB: dest = Sn*Sm - Sa

    // ---- Conversions ----
    ScvtfSW, ScvtfDW,   // signed int32 → float
    ScvtfSX, ScvtfDX,   // signed int64 → float
    FcvtzsWS, FcvtzsWD, // float → int32
    FcvtzsXS, FcvtzsXD, // float → int64
    FcvtSD, FcvtDS,     // float↔double

    // ---- Move ----
    Movz,        // MOVZ Xd, #imm16, LSL #shift
    Movk,        // MOVK Xd, #imm16, LSL #shift
    Movn,        // MOVN Xd, #imm16, LSL #shift
    MovReg,      // MOV Xd, Xm  (alias: ORR Xd, XZR, Xm)
    FmovReg,     // FMOV Dd, Dm

    // ---- Memory ----
    StrImm,      // STR Xt, [Xn, #imm]
    LdrImm,      // LDR Xt, [Xn, #imm]
    StrhImm,     // STRH Wt, [Xn, #imm]  (store 16-bit half)
    LdrshImm,    // LDRSH Wt, [Xn, #imm] (load 16-bit half, sign-extended)
    StrbImm,     // STRB Wt, [Xn, #imm]  (store 8-bit byte)
    LdrsbImm,    // LDRSB Wt, [Xn, #imm] (load 8-bit byte, sign-extended)
    StrFpImm,    // STR Dt, [Xn, #imm]  (float store)
    LdrFpImm,    // LDR Dt, [Xn, #imm]  (float load)
    StpPre,      // STP Xt1, Xt2, [Xn, #imm]!  (pre-index)
    LdpPost,     // LDP Xt1, Xt2, [Xn], #imm   (post-index)
    StpOffset,   // STP Xt1, Xt2, [Xn, #imm]   (signed offset, no writeback)
    LdpOffset,   // LDP Xt1, Xt2, [Xn, #imm]   (signed offset, no writeback)
    AdrpLdr,     // ADRP + LDR sequence (load value from PC-relative address)
    AdrpAdd,     // ADRP + ADD sequence (compute PC-relative address)

    // ---- Branch ----
    B,           // B label
    BCond,       // B.cond label
    Bl,          // BL label  (call)
    Ret,         // RET

    // ---- Extend ----
    Sxtw,        // SXTW Xd, Wn (sign-extend 32→64)
    Sxth,        // SXTH Wd|Xd, Wn (sign-extend 16→32 or 16→64)
    Sxtb,        // SXTB Wd|Xd, Wn (sign-extend 8→32 or 8→64)

    // ---- Special ----
    Nop,
    Brk,         // BRK #imm16  (debug trap)
}

/// ARM64 condition codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmCond {
    Eq, Ne,
    Hs, Lo,      // unsigned >=, <
    Mi, Pl,      // negative, positive
    Hi, Ls,      // unsigned >, <=
    Ge, Lt,      // signed >=, <
    Gt, Le,      // signed >, <=
}

/// A machine operand.
#[derive(Debug, Clone, PartialEq)]
pub enum MachineOperand {
    /// Virtual register.
    VReg(VRegId),
    /// Physical register (post-allocation or fixed registers like SP, FP, LR).
    PhysReg(PhysReg),
    /// Immediate value.
    Imm(i64),
    /// Stack frame slot (offset from FP).
    FrameSlot(i32),
    /// Condition code.
    Cond(ArmCond),
    /// Reference to a machine block (branch target).
    BlockRef(MBlockId),
    /// External symbol name (for BL to functions).
    Extern(String),
    /// Module-level global by name. Used by ADRP+ADD for SAVE'd
    /// locals and module variables, where the operand resolves to
    /// `_globalname@PAGE` / `_globalname@PAGEOFF` at emit time.
    GlobalLabel(String),
    /// Constant pool entry index.
    ConstPool(u32),
    /// Shift amount for MOVZ/MOVK (0, 16, 32, 48).
    Shift(u8),
}

/// Physical register reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhysReg {
    /// 64-bit general purpose register (X0-X30).
    Gp(u8),
    /// 32-bit general purpose register (W0-W30).
    Gp32(u8),
    /// 64-bit FP/SIMD register (D0-D31).
    Fp(u8),
    /// 32-bit FP/SIMD register (S0-S31).
    Fp32(u8),
    /// Stack pointer.
    Sp,
    /// Zero register (64-bit context).
    Xzr,
    /// Zero register (32-bit context).
    Wzr,
}

impl PhysReg {
    pub const FP: PhysReg = PhysReg::Gp(29);
    pub const LR: PhysReg = PhysReg::Gp(30);
}

/// Machine block identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MBlockId(pub u32);

/// A machine instruction.
#[derive(Debug, Clone)]
pub struct MachineInst {
    pub opcode: ArmOpcode,
    pub operands: Vec<MachineOperand>,
    /// Virtual register defined by this instruction (if any).
    pub def: Option<VRegId>,
}

/// A machine basic block.
#[derive(Debug, Clone)]
pub struct MachineBlock {
    pub id: MBlockId,
    pub label: String,
    pub insts: Vec<MachineInst>,
}

impl MachineBlock {
    pub fn new(id: MBlockId, label: String) -> Self {
        Self { id, label, insts: Vec::new() }
    }
}

/// Constant pool entry.
#[derive(Debug, Clone)]
pub enum ConstPoolEntry {
    F32(f32),
    F64(f64),
    I64(i64),
    Bytes(Vec<u8>),
}

/// Stack frame layout.
#[derive(Debug, Clone)]
pub struct StackFrame {
    /// Slots for local variables (name → offset from FP).
    pub locals: Vec<FrameSlot>,
    /// Total frame size in bytes (16-byte aligned).
    pub size: u32,
    /// Offset of the next available local slot.
    next_offset: i32,
    /// Maximum outgoing stack argument area reserved at the bottom of the frame.
    outgoing_arg_size: u32,
}

/// A stack frame slot.
#[derive(Debug, Clone)]
pub struct FrameSlot {
    pub offset: i32,  // negative offset from FP
    pub size: u32,    // size in bytes
}

impl StackFrame {
    pub fn new() -> Self {
        // Apple ARM64 frame layout:
        //   FP points at saved FP/LR (top of frame).
        //   Locals are at negative offsets from FP.
        //   SP is at the bottom of the frame.
        //
        //   [FP+0]  = saved x29
        //   [FP+8]  = saved x30
        //   [FP-8]  = first local
        //   [FP-16] = second local
        //   ...
        //   [SP]    = bottom of frame
        //
        // Prologue: sub sp, sp, #FRAME_SIZE
        //           stp x29, x30, [sp, #FRAME_SIZE - 16]
        //           add x29, sp, #FRAME_SIZE - 16
        // Epilogue: ldp x29, x30, [sp, #FRAME_SIZE - 16]
        //           add sp, sp, #FRAME_SIZE
        //           ret
        Self { locals: Vec::new(), size: 16, next_offset: 0, outgoing_arg_size: 0 }
    }
}

impl Default for StackFrame {
    fn default() -> Self { Self::new() }
}

impl StackFrame {
    /// Allocate a local variable slot. Returns a negative offset from FP.
    /// Locals grow downward from FP: first local at [FP-8], etc.
    pub fn alloc_local(&mut self, size: u32) -> i32 {
        let align = if size >= 8 { 8i32 } else { 4 };
        self.next_offset += size as i32;
        // Align the running offset.
        self.next_offset = (self.next_offset + align - 1) & !(align - 1);
        let offset = -self.next_offset; // negative from FP
        self.locals.push(FrameSlot { offset, size });
        self.recompute_size();
        offset
    }

    /// Frame size = 16 (FP+LR) + locals, 16-byte aligned.
    fn recompute_size(&mut self) {
        let raw = 16 + self.next_offset as u32 + self.outgoing_arg_size;
        self.size = (raw + 15) & !15;
    }

    /// Reserve the maximum outgoing stack argument area this function needs.
    pub fn reserve_outgoing_args(&mut self, size: u32) {
        if size > self.outgoing_arg_size {
            self.outgoing_arg_size = size;
            self.recompute_size();
        }
    }
}

/// A machine function — the codegen output for one IR function.
#[derive(Debug, Clone)]
pub struct MachineFunction {
    pub name: String,
    pub blocks: Vec<MachineBlock>,
    pub frame: StackFrame,
    pub vregs: Vec<VReg>,
    pub const_pool: Vec<ConstPoolEntry>,
    next_vreg: u32,
    next_block: u32,
}

impl MachineFunction {
    pub fn new(name: String) -> Self {
        let entry = MachineBlock::new(MBlockId(0), format!("_{}", name));
        Self {
            name,
            blocks: vec![entry],
            frame: StackFrame::new(),
            vregs: Vec::new(),
            const_pool: Vec::new(),
            next_vreg: 0,
            next_block: 1,
        }
    }

    /// Allocate a new virtual register.
    pub fn new_vreg(&mut self, class: RegClass) -> VRegId {
        let id = VRegId(self.next_vreg);
        self.next_vreg += 1;
        self.vregs.push(VReg { id, class });
        id
    }

    /// Create a new machine block.
    pub fn new_block(&mut self, label: &str) -> MBlockId {
        let id = MBlockId(self.next_block);
        self.next_block += 1;
        self.blocks.push(MachineBlock::new(id, label.into()));
        id
    }

    /// Get a block by ID.
    pub fn block(&self, id: MBlockId) -> &MachineBlock {
        self.blocks.iter().find(|b| b.id == id).expect("machine block not found")
    }

    /// Get a mutable block by ID.
    pub fn block_mut(&mut self, id: MBlockId) -> &mut MachineBlock {
        self.blocks.iter_mut().find(|b| b.id == id).expect("machine block not found")
    }

    /// Add a constant pool entry, return its index.
    pub fn add_const(&mut self, entry: ConstPoolEntry) -> u32 {
        let idx = self.const_pool.len() as u32;
        self.const_pool.push(entry);
        idx
    }

    /// Allocate a local stack slot.
    pub fn alloc_local(&mut self, size: u32) -> i32 {
        self.frame.alloc_local(size)
    }

    /// Reserve outgoing stack argument space for calls made by this function.
    pub fn reserve_outgoing_args(&mut self, size: u32) {
        self.frame.reserve_outgoing_args(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_frame_alignment() {
        let mut frame = StackFrame::new();
        frame.alloc_local(4); // i32
        assert_eq!(frame.size % 16, 0, "frame size {} not 16-byte aligned", frame.size);

        frame.alloc_local(8); // i64
        assert_eq!(frame.size % 16, 0);

        frame.alloc_local(1); // bool
        assert_eq!(frame.size % 16, 0);
    }

    #[test]
    fn stack_slots_dont_overlap() {
        let mut frame = StackFrame::new();
        let off1 = frame.alloc_local(4);
        let off2 = frame.alloc_local(4);
        let off3 = frame.alloc_local(8);
        assert_ne!(off1, off2);
        assert_ne!(off2, off3);
        // All offsets should be negative (below FP).
        assert!(off1 < 0);
        assert!(off2 < 0);
        assert!(off3 < 0);
        // No overlap: each slot's range is [offset, offset+size).
        assert!(off2 + 4 <= off1 || off1 + 4 <= off2);
    }

    #[test]
    fn vreg_allocation() {
        let mut mf = MachineFunction::new("test".into());
        let v0 = mf.new_vreg(RegClass::Gp64);
        let v1 = mf.new_vreg(RegClass::Fp64);
        assert_eq!(v0, VRegId(0));
        assert_eq!(v1, VRegId(1));
        assert_eq!(mf.vregs.len(), 2);
        assert_eq!(mf.vregs[0].class, RegClass::Gp64);
        assert_eq!(mf.vregs[1].class, RegClass::Fp64);
    }

    #[test]
    fn const_pool() {
        let mut mf = MachineFunction::new("test".into());
        let idx0 = mf.add_const(ConstPoolEntry::F64(3.14));
        let idx1 = mf.add_const(ConstPoolEntry::F32(2.0));
        assert_eq!(idx0, 0);
        assert_eq!(idx1, 1);
    }

    #[test]
    fn frame_size_starts_at_16() {
        let frame = StackFrame::new();
        assert_eq!(frame.size, 16); // just FP+LR
    }

    #[test]
    fn reserve_outgoing_args_grows_frame() {
        let mut frame = StackFrame::new();
        frame.alloc_local(8);
        let before = frame.size;
        frame.reserve_outgoing_args(16);
        assert!(frame.size >= before + 16);
        assert_eq!(frame.size % 16, 0);
    }
}
