//! x86_64 instruction selection (sprint x05).
//!
//! Translates SSA IR into x86 MIR over virtual registers, mirroring
//! the ARM64 isel architecture (phased vreg pre-allocation, block
//! maps, edge copies). Output stays three-address: destructive-operand
//! ties are NOT satisfied here — `twoaddr::convert_to_two_address`
//! rewrites `d = op a, b` into `mov d, a; d = op d, b` before register
//! allocation. Fixed-register instructions (idiv, variable shifts) and
//! call arguments are modeled with explicit `Reg(..)` operands, the
//! same precedent ARM isel set with `PhysReg` call-argument moves.
//!
//! Scope is what -O0 Fortran integer/FP scalar programs need; vector
//! ops, i128 values, indirect calls, and aggregate ABI shapes panic
//! with "x05 scope" messages.
//!
//! Not wired into `codegen::emit_module` yet — regalloc and frame
//! layout (later in x05) must run before the printer accepts this
//! output (it panics on VReg/FrameSlot operands by design).
//!
//! Width convention, mirroring ARM's Gp32: i8/i16/i32 and Bool values
//! live sign-extended (Bool/zero) in 32-bit registers and ALU ops run
//! at `OpSize::L`; only loads/stores touch memory at the narrow width.

use super::abi::{
    classify_abi_arg, classify_return, AbiField, AbiTy, X86AbiArgLoc, X86AbiState, X86RetLoc,
};
use super::mir::*;
use crate::ir::inst::*;
use crate::ir::types::*;
use std::collections::HashMap;

/// Select machine instructions for an entire IR module.
pub fn select_module(module: &Module) -> Vec<X86Function> {
    let func_names: Vec<String> = module.functions.iter().map(|f| f.name.clone()).collect();
    module
        .functions
        .iter()
        .map(|f| select_function(f, &func_names, module.layout))
        .collect()
}

/// Select machine instructions for one IR function. `func_names`
/// resolves `FuncRef::Internal` indices — ELF symbol naming is
/// identity, so the name goes straight into an `Extern` operand
/// (no `_func_N` placeholder pass like ARM's Mach-O path needs).
pub fn select_function(
    func: &Function,
    func_names: &[String],
    layout: crate::target::TargetLayout,
) -> X86Function {
    let mut mf = X86Function::new(func.name.clone());
    let mut ctx = X86ISelCtx::new(layout);

    // Phase 1: frame slots for every IR alloca.
    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::Alloca(ty) = &inst.kind {
                let size = alloca_size(ty, layout);
                let align = slot_align(size);
                let slot = mf.alloc_frame_slot(size, align);
                ctx.alloca_slots.insert(inst.id, slot);
            }
        }
    }

    // Phase 2: machine blocks for IR blocks. Entry is MBlockId(0).
    ctx.block_map.insert(func.entry, MBlockId(0));
    for block in &func.blocks {
        if block.id != func.entry {
            let mb_id = mf.new_block();
            ctx.block_map.insert(block.id, mb_id);
        }
    }

    // Phase 2.5: classify incoming parameters and reserve their vregs.
    let mut abi_state = X86AbiState::new();
    let mut param_info: Vec<(X86VReg, X86AbiArgLoc, IrType)> = Vec::new();
    let mut wide_param_info: Vec<((i32, i32), X86AbiArgLoc)> = Vec::new();
    for param in &func.params {
        let loc = classify_abi_arg(&abi_ty_of(&param.ty), &mut abi_state);
        if is_wide_pair_ty(&param.ty) {
            let slots = (mf.alloc_frame_slot(8, 8), mf.alloc_frame_slot(8, 8));
            ctx.wide_slots.insert(param.id, slots);
            wide_param_info.push((slots, loc));
            continue;
        }
        let vreg = mf.new_vreg(type_to_class(&param.ty));
        ctx.value_map.insert(param.id, vreg);
        param_info.push((vreg, loc, param.ty.clone()));
    }

    // Phase 3: receive incoming arguments into param vregs. No
    // prologue here — frame layout (later in x05) inserts it.
    for (vreg, loc, ty) in &param_info {
        match loc {
            X86AbiArgLoc::Gp(r) => {
                let size = gp_move_size(vreg.class);
                push(
                    &mut mf,
                    MBlockId(0),
                    X86Opcode::MovRR,
                    size,
                    vec![X86Operand::Reg(*r)],
                    Some(X86Operand::VReg(*vreg)),
                );
            }
            X86AbiArgLoc::Xmm(r) => {
                push(
                    &mut mf,
                    MBlockId(0),
                    fp_move(ty),
                    fp_size(ty),
                    vec![X86Operand::Reg(*r)],
                    Some(X86Operand::VReg(*vreg)),
                );
            }
            X86AbiArgLoc::Stack { offset, .. } => {
                // After the (future) `push rbp; mov rsp, rbp` prologue:
                // saved rbp at rbp+0, return address at rbp+8, first
                // stack argument at rbp+16.
                let mem = X86Operand::Mem {
                    base: Some(X86Reg::Rbp),
                    index: None,
                    scale: 1,
                    disp: 16 + offset,
                };
                emit_load(&mut mf, MBlockId(0), *vreg, ty, mem);
            }
            other => panic!(
                "isel: unexpected ABI loc {:?} for incoming scalar param",
                other
            ),
        }
    }

    // Phase 3.5: spill incoming i128 pairs to their wide slots. The
    // SysV pair arrives as (lo, hi) in two consecutive GP registers;
    // stack-passed i128 sits at rbp+16+offset, 16-byte aligned
    // (abi.tex §3.2.3: INTEGER class, two eightbytes).
    for (slots, loc) in &wide_param_info {
        match loc {
            X86AbiArgLoc::GpPair(lo, hi) => {
                emit_store_pair_to_slot(&mut mf, MBlockId(0), *slots, *lo, *hi);
            }
            X86AbiArgLoc::XmmPair(a, b) => {
                // complex(8) param: xmm pair into the wide slots.
                for (slot, xmm) in [(slots.0, *a), (slots.1, *b)] {
                    push(
                        &mut mf,
                        MBlockId(0),
                        X86Opcode::Movsd,
                        OpSize::Q,
                        vec![X86Operand::Reg(xmm)],
                        Some(X86Operand::FrameSlot(slot)),
                    );
                }
            }
            X86AbiArgLoc::Stack { offset, .. } => {
                for (half, scratch) in [(0i64, X86Reg::Rax), (8, X86Reg::Rdx)] {
                    push(
                        &mut mf,
                        MBlockId(0),
                        X86Opcode::MovRM,
                        OpSize::Q,
                        vec![X86Operand::Mem {
                            base: Some(X86Reg::Rbp),
                            index: None,
                            scale: 1,
                            disp: 16 + offset + half,
                        }],
                        Some(X86Operand::Reg(scratch)),
                    );
                }
                emit_store_pair_to_slot(&mut mf, MBlockId(0), *slots, X86Reg::Rax, X86Reg::Rdx);
            }
            other => panic!("isel: unexpected ABI loc {:?} for incoming i128", other),
        }
    }

    // Phase 4a: reserve vregs for every block param and instruction
    // result before walking any instructions — branch terminators need
    // target-param vregs, and SSA dominance allows uses that precede
    // the defining block in vec order (same rationale as ARM isel).
    for block in &func.blocks {
        for bp in &block.params {
            if is_wide_pair_ty(&bp.ty) {
                let slots = (mf.alloc_frame_slot(8, 8), mf.alloc_frame_slot(8, 8));
                ctx.wide_slots.insert(bp.id, slots);
                continue;
            }
            reject_unsupported(&bp.ty);
            let vreg = mf.new_vreg(type_to_class(&bp.ty));
            ctx.value_map.insert(bp.id, vreg);
        }
        for inst in &block.insts {
            if matches!(inst.ty, IrType::Void) {
                continue;
            }
            if is_wide_pair_ty(&inst.ty) {
                let slots = (mf.alloc_frame_slot(8, 8), mf.alloc_frame_slot(8, 8));
                ctx.wide_slots.insert(inst.id, slots);
                continue;
            }
            reject_unsupported(&inst.ty);
            let vreg = mf.new_vreg(type_to_class(&inst.ty));
            ctx.value_map.insert(inst.id, vreg);
        }
    }

    // Snapshot block params so terminator selection can read each
    // target's params while holding &mut X86Function.
    for block in &func.blocks {
        ctx.block_params.insert(block.id, block.params.clone());
    }

    // Phase 4b: select instructions and terminators. `mb` is a cursor:
    // Select lowers to a diamond and leaves the cursor on its join
    // block, so the rest of the IR block continues there.
    for block in &func.blocks {
        let mut mb = ctx.block_map[&block.id];
        for inst in &block.insts {
            mb = select_inst(&mut mf, &mut ctx, mb, inst, func, func_names);
        }
        if let Some(term) = &block.terminator {
            select_terminator(&mut mf, &mut ctx, mb, term, func);
        }
    }

    mf
}

/// Instruction selection context.
struct X86ISelCtx {
    /// Target layout of the module being selected (x02).
    layout: crate::target::TargetLayout,
    /// IR ValueId → MIR vreg.
    value_map: HashMap<ValueId, X86VReg>,
    /// IR BlockId → MIR MBlockId.
    block_map: HashMap<BlockId, MBlockId>,
    /// IR alloca ValueId → frame slot id.
    alloca_slots: HashMap<ValueId, i32>,
    /// IR BlockId → its block params, snapshotted before phase 4b.
    block_params: HashMap<BlockId, Vec<BlockParam>>,
    /// IR i128 ValueId → (lo, hi) frame slots. i128 values are
    /// memory-resident (the arm64 isel's wide-slot model) as TWO
    /// independent 8-byte slots — every producer stores a quad pair,
    /// every consumer loads through physical scratches (rax:rdx), and
    /// no i128 ever occupies a vreg. Two slots instead of one
    /// 16-byte slot keeps the operand grammar untouched: each half is
    /// an ordinary FrameSlot. Nothing requires the halves adjacent;
    /// ABI crossings (args, returns, memory) always stage via the
    /// register pair.
    wide_slots: HashMap<ValueId, (i32, i32)>,
}

impl X86ISelCtx {
    fn new(layout: crate::target::TargetLayout) -> Self {
        Self {
            layout,
            value_map: HashMap::new(),
            block_map: HashMap::new(),
            alloca_slots: HashMap::new(),
            block_params: HashMap::new(),
            wide_slots: HashMap::new(),
        }
    }

    /// Get the vreg for an IR value, assuming phase 4a mapped it.
    fn lookup_vreg(&self, val: ValueId) -> X86VReg {
        *self.value_map.get(&val).unwrap_or_else(|| {
            panic!(
                "isel: unmapped IR value %{} — phase 4a should have allocated \
                 a vreg for every IR value before phase 4b runs",
                val.0
            )
        })
    }

    /// (lo, hi) frame slots of an i128 value (allocated in 2.5/4a).
    fn lookup_wide_slot(&self, val: ValueId) -> (i32, i32) {
        *self.wide_slots.get(&val).unwrap_or_else(|| {
            panic!(
                "isel: i128 value %{} has no wide slots — phase 4a should have \
                 allocated them",
                val.0
            )
        })
    }

    /// Get machine block for an IR block.
    fn lookup_block(&self, block: BlockId) -> MBlockId {
        *self.block_map.get(&block).unwrap_or(&MBlockId(0))
    }

    /// Address operand for a Load/Store/Lea base: allocas become
    /// `FrameSlot`, anything else is the documented address-in-vreg
    /// form (see `X86Opcode::MovRM`).
    fn addr_operand(&self, addr: ValueId) -> X86Operand {
        if let Some(&slot) = self.alloca_slots.get(&addr) {
            X86Operand::FrameSlot(slot)
        } else {
            X86Operand::VReg(self.lookup_vreg(addr))
        }
    }
}

fn push(
    mf: &mut X86Function,
    mb: MBlockId,
    opcode: X86Opcode,
    size: OpSize,
    operands: Vec<X86Operand>,
    def: Option<X86Operand>,
) {
    mf.block_mut(mb).insts.push(X86Inst {
        opcode,
        size,
        operands,
        def,
    });
}

/// How one IR FP comparison maps onto `ucomis{s,d}` + flags (the
/// sprint-doc condition table, fixed here once and unit-tested below).
/// Unordered (NaN) comparison sets ZF=PF=CF=1, so:
///  - `>` / `>=` use A/AE — CF-based, false on NaN.
///  - `<` / `<=` SWAP the ucomi operands then use A/AE; B/BE read CF
///    and would come out true on NaN.
///  - `==` is `sete` AND `setnp` (NP masks the unordered case);
///    `/=` is `setne` OR `setp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FpCmpPlan {
    Direct {
        swap_operands: bool,
        cond: X86Cond,
    },
    Equality {
        cond: X86Cond,
        parity: X86Cond,
        combine: X86Opcode,
    },
}

pub(crate) fn fp_cond_plan(op: CmpOp) -> FpCmpPlan {
    match op {
        CmpOp::Gt => FpCmpPlan::Direct {
            swap_operands: false,
            cond: X86Cond::A,
        },
        CmpOp::Ge => FpCmpPlan::Direct {
            swap_operands: false,
            cond: X86Cond::Ae,
        },
        CmpOp::Lt => FpCmpPlan::Direct {
            swap_operands: true,
            cond: X86Cond::A,
        },
        CmpOp::Le => FpCmpPlan::Direct {
            swap_operands: true,
            cond: X86Cond::Ae,
        },
        CmpOp::Eq => FpCmpPlan::Equality {
            cond: X86Cond::E,
            parity: X86Cond::Np,
            combine: X86Opcode::And,
        },
        CmpOp::Ne => FpCmpPlan::Equality {
            cond: X86Cond::Ne,
            parity: X86Cond::P,
            combine: X86Opcode::Or,
        },
    }
}

/// IR CmpOp → integer condition code. Fortran integers are signed, and
/// IR CmpOp is the signed comparison (same mapping ARM's
/// `cmp_to_arm_cond` makes).
fn int_cond(op: CmpOp) -> X86Cond {
    match op {
        CmpOp::Eq => X86Cond::E,
        CmpOp::Ne => X86Cond::Ne,
        CmpOp::Lt => X86Cond::L,
        CmpOp::Le => X86Cond::Le,
        CmpOp::Gt => X86Cond::G,
        CmpOp::Ge => X86Cond::Ge,
    }
}

/// Select machine instructions for a single IR instruction. Returns
/// the machine block selection should continue in (changes only when
/// Select opens a diamond).
fn select_inst(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    func: &Function,
    func_names: &[String],
) -> MBlockId {
    // ---- wide-pair early dispatch (x08): i128 and 16-byte arrays ----
    if is_wide_pair_ty(&inst.ty) {
        use X86Reg::{Rax, Rcx, Rdx};
        match &inst.kind {
            InstKind::ConstInt(val, IntWidth::I128) => {
                let dest = ctx.lookup_wide_slot(inst.id);
                emit_const_i128_pair(mf, mb, *val, Rax, Rdx);
                emit_store_pair_to_slot(mf, mb, dest, Rax, Rdx);
                return mb;
            }
            InstKind::Undef(_) => {
                let dest = ctx.lookup_wide_slot(inst.id);
                emit_const_i128_pair(mf, mb, 0, Rax, Rdx);
                emit_store_pair_to_slot(mf, mb, dest, Rax, Rdx);
                return mb;
            }
            InstKind::IAdd(a, b) | InstKind::ISub(a, b) => {
                let dest = ctx.lookup_wide_slot(inst.id);
                let lhs = ctx.lookup_wide_slot(*a);
                let rhs = ctx.lookup_wide_slot(*b);
                emit_load_slot_pair(mf, mb, lhs, Rax, Rdx);
                let (lo_op, hi_op) = if matches!(inst.kind, InstKind::IAdd(..)) {
                    (X86Opcode::Add, X86Opcode::Adc)
                } else {
                    (X86Opcode::Sub, X86Opcode::Sbb)
                };
                // add/adc (sub/sbb) MUST stay adjacent: both operands
                // are phys regs + frame slots, so the allocator
                // inserts nothing between them.
                emit_alu_reg_slot(mf, mb, lo_op, Rax, rhs.0);
                emit_alu_reg_slot(mf, mb, hi_op, Rdx, rhs.1);
                emit_store_pair_to_slot(mf, mb, dest, Rax, Rdx);
                return mb;
            }
            InstKind::INeg(a) => {
                let dest = ctx.lookup_wide_slot(inst.id);
                let src = ctx.lookup_wide_slot(*a);
                emit_load_slot_pair(mf, mb, src, Rax, Rdx);
                // 128-bit two's-complement negate:
                //   negq hi; negq lo; sbbq $0, hi
                // negq lo sets CF iff lo != 0; hi = -hi - CF.
                push(
                    mf,
                    mb,
                    X86Opcode::Neg,
                    OpSize::Q,
                    vec![X86Operand::Reg(Rdx)],
                    Some(X86Operand::Reg(Rdx)),
                );
                push(
                    mf,
                    mb,
                    X86Opcode::Neg,
                    OpSize::Q,
                    vec![X86Operand::Reg(Rax)],
                    Some(X86Operand::Reg(Rax)),
                );
                push(
                    mf,
                    mb,
                    X86Opcode::Sbb,
                    OpSize::Q,
                    vec![X86Operand::Reg(Rdx), X86Operand::Imm(0)],
                    Some(X86Operand::Reg(Rdx)),
                );
                emit_store_pair_to_slot(mf, mb, dest, Rax, Rdx);
                return mb;
            }
            InstKind::Load(addr) => {
                let dest = ctx.lookup_wide_slot(inst.id);
                match ctx.addr_operand(*addr) {
                    X86Operand::FrameSlot(base_slot) => {
                        // An alloca'd i128: its storage is one 16-byte
                        // alloca slot; load (lo, hi) from +0/+8 of it.
                        // FrameSlot has no displacement, so go through
                        // rcx as a pointer.
                        push(
                            mf,
                            mb,
                            X86Opcode::Lea,
                            OpSize::Q,
                            vec![X86Operand::FrameSlot(base_slot)],
                            Some(X86Operand::Reg(Rcx)),
                        );
                    }
                    vreg_op => {
                        // Pointer in a vreg: pull the pointer VALUE
                        // into rcx (MovRR; the allocator loads the
                        // vreg into a scratch and moves it over).
                        push(
                            mf,
                            mb,
                            X86Opcode::MovRR,
                            OpSize::Q,
                            vec![vreg_op],
                            Some(X86Operand::Reg(Rcx)),
                        );
                    }
                }
                for (disp, reg) in [(0i64, Rax), (8, Rdx)] {
                    push(
                        mf,
                        mb,
                        X86Opcode::MovRM,
                        OpSize::Q,
                        vec![X86Operand::Mem {
                            base: Some(Rcx),
                            index: None,
                            scale: 1,
                            disp,
                        }],
                        Some(X86Operand::Reg(reg)),
                    );
                }
                emit_store_pair_to_slot(mf, mb, dest, Rax, Rdx);
                return mb;
            }
            InstKind::Select(cond, tv, fv) => {
                // Same diamond shape as the narrow Select below, but
                // copying slot pairs.
                let dest = ctx.lookup_wide_slot(inst.id);
                let ts = ctx.lookup_wide_slot(*tv);
                let fs = ctx.lookup_wide_slot(*fv);
                let vcond = ctx.lookup_vreg(*cond);
                let false_mb = mf.new_block();
                let join_mb = mf.new_block();
                push(
                    mf,
                    mb,
                    X86Opcode::Test,
                    OpSize::L,
                    vec![X86Operand::VReg(vcond), X86Operand::VReg(vcond)],
                    None,
                );
                push(
                    mf,
                    mb,
                    X86Opcode::Jcc,
                    OpSize::Q,
                    vec![X86Operand::Cond(X86Cond::E), X86Operand::BlockRef(false_mb)],
                    None,
                );
                emit_load_slot_pair(mf, mb, ts, Rax, Rdx);
                emit_store_pair_to_slot(mf, mb, dest, Rax, Rdx);
                push(
                    mf,
                    mb,
                    X86Opcode::Jmp,
                    OpSize::Q,
                    vec![X86Operand::BlockRef(join_mb)],
                    None,
                );
                emit_load_slot_pair(mf, false_mb, fs, Rax, Rdx);
                emit_store_pair_to_slot(mf, false_mb, dest, Rax, Rdx);
                push(
                    mf,
                    false_mb,
                    X86Opcode::Jmp,
                    OpSize::Q,
                    vec![X86Operand::BlockRef(join_mb)],
                    None,
                );
                return join_mb;
            }
            InstKind::Call(..) | InstKind::RuntimeCall(..) => {
                // The call selector stores the rax:rdx return pair to
                // the wide slot itself.
                select_call_inst(mf, ctx, mb, inst, func, func_names);
                return mb;
            }
            other => panic!("x08 scope: i128 instruction {:?} not selected yet", other),
        }
    }
    // i128 with non-i128 result types: stores, compares, returns of
    // i128 OPERANDS are handled in their own arms below.

    match &inst.kind {
        // ---- Constants ----
        InstKind::ConstInt(val, width) => {
            let dest = ctx.lookup_vreg(inst.id);
            let size = match width {
                IntWidth::I64 => OpSize::Q,
                IntWidth::I128 => panic!("x05 scope: i128 constants deferred"),
                _ => OpSize::L,
            };
            // i64 immediates that need movabs are still MovRI with Q;
            // the printer handles the spelling.
            push(
                mf,
                mb,
                X86Opcode::MovRI,
                size,
                vec![X86Operand::Imm(*val as i64)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::ConstFloat(val, width) => {
            let dest = ctx.lookup_vreg(inst.id);
            let bits = match width {
                FloatWidth::F32 => (*val as f32).to_bits() as u64,
                FloatWidth::F64 => val.to_bits(),
            };
            let label = mf.add_rodata(bits);
            push(
                mf,
                mb,
                fp_move_w(*width),
                fp_size_w(*width),
                vec![X86Operand::RipLabel(label)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::ConstBool(val) => {
            let dest = ctx.lookup_vreg(inst.id);
            push(
                mf,
                mb,
                X86Opcode::MovRI,
                OpSize::L,
                vec![X86Operand::Imm(if *val { 1 } else { 0 })],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::ConstString(bytes) => {
            // Address of an interned .rodata blob; the consuming
            // runtime call carries the length separately (same shape
            // as ARM's AdrpAdd const-pool lowering).
            let dest = ctx.lookup_vreg(inst.id);
            let label = mf.add_rodata_bytes(bytes);
            mf.block_mut(mb).insts.push(X86Inst {
                opcode: X86Opcode::Lea,
                size: OpSize::Q,
                operands: vec![X86Operand::RipLabel(label)],
                def: Some(X86Operand::VReg(dest)),
            });
        }

        InstKind::Undef(_) => {
            // Deterministic zero, same rationale as ARM isel: a truly
            // undefined vreg leaks whatever was in the assigned
            // register, making opt-level diffs nondeterministic.
            let dest = ctx.lookup_vreg(inst.id);
            match dest.class {
                X86RegClass::Xmm => {
                    let label = mf.add_rodata(0);
                    push(
                        mf,
                        mb,
                        X86Opcode::Movsd,
                        OpSize::Q,
                        vec![X86Operand::RipLabel(label)],
                        Some(X86Operand::VReg(dest)),
                    );
                }
                _ => {
                    let size = gp_move_size(dest.class);
                    push(
                        mf,
                        mb,
                        X86Opcode::MovRI,
                        size,
                        vec![X86Operand::Imm(0)],
                        Some(X86Operand::VReg(dest)),
                    );
                }
            }
        }

        // ---- Integer arithmetic (three-address; twoaddr ties later) ----
        InstKind::IAdd(a, b) => emit_int_binop(mf, ctx, mb, inst, X86Opcode::Add, *a, *b),
        InstKind::ISub(a, b) => emit_int_binop(mf, ctx, mb, inst, X86Opcode::Sub, *a, *b),
        InstKind::IMul(a, b) => emit_int_binop(mf, ctx, mb, inst, X86Opcode::Imul, *a, *b),
        InstKind::IDiv(a, b) => emit_div_mod(mf, ctx, mb, inst, *a, *b, X86Reg::Rax),
        InstKind::IMod(a, b) => {
            // The idiv remainder takes the dividend's sign — exactly
            // Fortran MOD's semantics, so %rdx is the result directly.
            emit_div_mod(mf, ctx, mb, inst, *a, *b, X86Reg::Rdx)
        }
        InstKind::INeg(a) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            push(
                mf,
                mb,
                X86Opcode::Neg,
                op_size(&inst.ty),
                vec![X86Operand::VReg(va)],
                Some(X86Operand::VReg(dest)),
            );
        }

        // ---- Float arithmetic ----
        InstKind::FAdd(a, b) => emit_fp_binop(
            mf,
            ctx,
            mb,
            inst,
            *a,
            *b,
            (X86Opcode::Addss, X86Opcode::Addsd),
        ),
        InstKind::FSub(a, b) => emit_fp_binop(
            mf,
            ctx,
            mb,
            inst,
            *a,
            *b,
            (X86Opcode::Subss, X86Opcode::Subsd),
        ),
        InstKind::FMul(a, b) => emit_fp_binop(
            mf,
            ctx,
            mb,
            inst,
            *a,
            *b,
            (X86Opcode::Mulss, X86Opcode::Mulsd),
        ),
        InstKind::FDiv(a, b) => emit_fp_binop(
            mf,
            ctx,
            mb,
            inst,
            *a,
            *b,
            (X86Opcode::Divss, X86Opcode::Divsd),
        ),
        InstKind::FNeg(a) => {
            // Flip the sign bit with a packed xor against a rodata
            // mask. The mask is loaded with movss/movsd, which zero
            // the upper lanes, so the packed op only changes the live
            // low lane of the (scalar-only) Xmm vreg.
            let (bits, opcode) = match float_width(&inst.ty) {
                FloatWidth::F32 => (0x8000_0000u64, X86Opcode::Xorps),
                FloatWidth::F64 => (0x8000_0000_0000_0000u64, X86Opcode::Xorpd),
            };
            emit_fp_mask_op(mf, ctx, mb, inst, *a, bits, opcode);
        }
        InstKind::FAbs(a) => {
            // Clear the sign bit; same mask-load reasoning as FNeg
            // (upper lanes AND with 0 → stay irrelevant zeros).
            let (bits, opcode) = match float_width(&inst.ty) {
                FloatWidth::F32 => (0x7fff_ffffu64, X86Opcode::Andps),
                FloatWidth::F64 => (0x7fff_ffff_ffff_ffffu64, X86Opcode::Andpd),
            };
            emit_fp_mask_op(mf, ctx, mb, inst, *a, bits, opcode);
        }
        InstKind::FSqrt(a) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let opcode = match float_width(&inst.ty) {
                FloatWidth::F32 => X86Opcode::Sqrtss,
                FloatWidth::F64 => X86Opcode::Sqrtsd,
            };
            push(
                mf,
                mb,
                opcode,
                fp_size(&inst.ty),
                vec![X86Operand::VReg(va)],
                Some(X86Operand::VReg(dest)),
            );
        }

        // ---- Packed SSE2 vector ops (x10). Only shapes the
        // SSE2_BASELINE vec_isa table admits reach here: float
        // mul/div/min/max/abs/fma, int add/sub/neg, i32 compares,
        // float compares, sum reductions for all four lane types,
        // float min/max reductions. Anything else is a table bug —
        // panic loudly rather than guess. ----
        InstKind::VLoad(ptr) => {
            let dest = ctx.lookup_vreg(inst.id);
            let vp = ctx.lookup_vreg(*ptr);
            push(
                mf,
                mb,
                X86Opcode::Movups,
                OpSize::Q,
                vec![X86Operand::VReg(vp)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VStore(vec, ptr) => {
            let vv = ctx.lookup_vreg(*vec);
            let vp = ctx.lookup_vreg(*ptr);
            // Store form: def carries the address vreg (the
            // is_fp_store_addr convention shared with Movss/Movsd).
            push(
                mf,
                mb,
                X86Opcode::Movups,
                OpSize::Q,
                vec![X86Operand::VReg(vv)],
                Some(X86Operand::VReg(vp)),
            );
        }
        InstKind::VAdd(a, b) | InstKind::VSub(a, b) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let is_add = matches!(inst.kind, InstKind::VAdd(..));
            let opcode = match vec_elem_of(&inst.ty) {
                IrType::Float(FloatWidth::F32) => {
                    if is_add {
                        X86Opcode::Addps
                    } else {
                        X86Opcode::Subps
                    }
                }
                IrType::Float(FloatWidth::F64) => {
                    if is_add {
                        X86Opcode::Addpd
                    } else {
                        X86Opcode::Subpd
                    }
                }
                IrType::Int(IntWidth::I32) => {
                    if is_add {
                        X86Opcode::Paddd
                    } else {
                        X86Opcode::Psubd
                    }
                }
                IrType::Int(IntWidth::I64) => {
                    if is_add {
                        X86Opcode::Paddq
                    } else {
                        X86Opcode::Psubq
                    }
                }
                other => panic!("x10: no packed add/sub for {:?}", other),
            };
            push(
                mf,
                mb,
                opcode,
                OpSize::Q,
                vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VMul(a, b) | InstKind::VDiv(a, b) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let is_mul = matches!(inst.kind, InstKind::VMul(..));
            let opcode = match vec_elem_of(&inst.ty) {
                IrType::Float(FloatWidth::F32) => {
                    if is_mul {
                        X86Opcode::Mulps
                    } else {
                        X86Opcode::Divps
                    }
                }
                IrType::Float(FloatWidth::F64) => {
                    if is_mul {
                        X86Opcode::Mulpd
                    } else {
                        X86Opcode::Divpd
                    }
                }
                // Integer mul/div are refused by the SSE2 vec_isa
                // table before vectorization.
                other => panic!("x10: packed mul/div on {:?} should not reach isel", other),
            };
            push(
                mf,
                mb,
                opcode,
                OpSize::Q,
                vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VMin(a, b) | InstKind::VMax(a, b) => {
            // Operand order is load-bearing for NaN: the vectorizer
            // builds VMax(t, f) from select(fcmp(t, f), t, f), where
            // NaN makes the compare false and yields f. min/max[ps|pd]
            // return the SECOND operand (src) on NaN; with dst = t
            // (tied operand 0) and src = f the packed op reproduces
            // the scalar select exactly.
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let is_min = matches!(inst.kind, InstKind::VMin(..));
            let opcode = match vec_elem_of(&inst.ty) {
                IrType::Float(FloatWidth::F32) => {
                    if is_min {
                        X86Opcode::Minps
                    } else {
                        X86Opcode::Maxps
                    }
                }
                IrType::Float(FloatWidth::F64) => {
                    if is_min {
                        X86Opcode::Minpd
                    } else {
                        X86Opcode::Maxpd
                    }
                }
                // Signed i32 has no native packed min/max at SSE2 —
                // synthesize compare-and-blend (x10c-1).
                IrType::Int(IntWidth::I32) => {
                    emit_packed_iminmax(mf, mb, is_min, va, vb, dest);
                    return mb;
                }
                other => panic!("x10: packed min/max on {:?} should not reach isel", other),
            };
            push(
                mf,
                mb,
                opcode,
                OpSize::Q,
                vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VSqrt(a) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let opcode = match vec_elem_of(&inst.ty) {
                IrType::Float(FloatWidth::F32) => X86Opcode::Sqrtps,
                IrType::Float(FloatWidth::F64) => X86Opcode::Sqrtpd,
                other => panic!("x10: packed sqrt on {:?} should not reach isel", other),
            };
            push(
                mf,
                mb,
                opcode,
                OpSize::Q,
                vec![X86Operand::VReg(va)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VNeg(a) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            match vec_elem_of(&inst.ty).clone() {
                IrType::Float(w) => {
                    // xor with the per-lane sign mask.
                    let mask = vec_mask_vreg(mf, ctx, mb, &w, true);
                    let opcode = match w {
                        FloatWidth::F32 => X86Opcode::Xorps,
                        FloatWidth::F64 => X86Opcode::Xorpd,
                    };
                    push(
                        mf,
                        mb,
                        opcode,
                        OpSize::Q,
                        vec![X86Operand::VReg(va), X86Operand::VReg(mask)],
                        Some(X86Operand::VReg(dest)),
                    );
                }
                IrType::Int(w) => {
                    // 0 - x: load a zero vector, psub the source.
                    let zero = mf.new_vreg(X86RegClass::Xmm128);
                    let label = mf.add_rodata_bytes(&[0u8; 16]);
                    push(
                        mf,
                        mb,
                        X86Opcode::Movups,
                        OpSize::Q,
                        vec![X86Operand::RipLabel(label)],
                        Some(X86Operand::VReg(zero)),
                    );
                    let opcode = match w {
                        IntWidth::I32 => X86Opcode::Psubd,
                        IntWidth::I64 => X86Opcode::Psubq,
                        other => panic!("x10: packed neg on {:?} lanes", other),
                    };
                    push(
                        mf,
                        mb,
                        opcode,
                        OpSize::Q,
                        vec![X86Operand::VReg(zero), X86Operand::VReg(va)],
                        Some(X86Operand::VReg(dest)),
                    );
                }
                other => panic!("x10: packed neg on {:?} should not reach isel", other),
            }
        }
        InstKind::VAbs(a) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            match vec_elem_of(&inst.ty).clone() {
                IrType::Float(w) => {
                    let mask = vec_mask_vreg(mf, ctx, mb, &w, false);
                    let opcode = match w {
                        FloatWidth::F32 => X86Opcode::Andps,
                        FloatWidth::F64 => X86Opcode::Andpd,
                    };
                    push(
                        mf,
                        mb,
                        opcode,
                        OpSize::Q,
                        vec![X86Operand::VReg(va), X86Operand::VReg(mask)],
                        Some(X86Operand::VReg(dest)),
                    );
                }
                // Signed i32 has no native packed abs at SSE2 (pabsd is
                // SSSE3) — synthesize sign-mask xor-sub (x10c-2).
                IrType::Int(IntWidth::I32) => emit_packed_iabs(mf, mb, va, dest),
                other => panic!("x10: packed abs on {:?} should not reach isel", other),
            }
        }
        InstKind::VFma(a, b, c) => {
            // SSE2 has no fused multiply-add; mul + add (two
            // roundings) matches what scalar x86 codegen produces for
            // the same source, preserving the O0-vs-O3 invariant.
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let vc = ctx.lookup_vreg(*c);
            let (mul_op, add_op) = match vec_elem_of(&inst.ty) {
                IrType::Float(FloatWidth::F32) => (X86Opcode::Mulps, X86Opcode::Addps),
                IrType::Float(FloatWidth::F64) => (X86Opcode::Mulpd, X86Opcode::Addpd),
                other => panic!("x10: packed fma on {:?} should not reach isel", other),
            };
            let prod = mf.new_vreg(X86RegClass::Xmm128);
            push(
                mf,
                mb,
                mul_op,
                OpSize::Q,
                vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
                Some(X86Operand::VReg(prod)),
            );
            push(
                mf,
                mb,
                add_op,
                OpSize::Q,
                vec![X86Operand::VReg(prod), X86Operand::VReg(vc)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VBroadcast(s) => {
            let dest = ctx.lookup_vreg(inst.id);
            let vs = ctx.lookup_vreg(*s);
            match vec_elem_of(&inst.ty).clone() {
                IrType::Float(FloatWidth::F32) => {
                    // Copy the scalar into a vector register, then
                    // splat lane 0 with shufps $0 (tied self-shuffle).
                    let tmp = mf.new_vreg(X86RegClass::Xmm128);
                    push(
                        mf,
                        mb,
                        X86Opcode::Movaps,
                        OpSize::Q,
                        vec![X86Operand::VReg(vs)],
                        Some(X86Operand::VReg(tmp)),
                    );
                    push(
                        mf,
                        mb,
                        X86Opcode::Shufps,
                        OpSize::Q,
                        vec![
                            X86Operand::VReg(tmp),
                            X86Operand::VReg(tmp),
                            X86Operand::Imm(0),
                        ],
                        Some(X86Operand::VReg(dest)),
                    );
                }
                IrType::Float(FloatWidth::F64) => {
                    let tmp = mf.new_vreg(X86RegClass::Xmm128);
                    push(
                        mf,
                        mb,
                        X86Opcode::Movaps,
                        OpSize::Q,
                        vec![X86Operand::VReg(vs)],
                        Some(X86Operand::VReg(tmp)),
                    );
                    push(
                        mf,
                        mb,
                        X86Opcode::Unpcklpd,
                        OpSize::Q,
                        vec![X86Operand::VReg(tmp), X86Operand::VReg(tmp)],
                        Some(X86Operand::VReg(dest)),
                    );
                }
                IrType::Int(w) => {
                    // movd/movq the GP scalar into lane 0, then
                    // splat: pshufd $0 for i32, punpcklqdq for i64.
                    let tmp = mf.new_vreg(X86RegClass::Xmm128);
                    let mv_size = match w {
                        IntWidth::I64 => OpSize::Q,
                        _ => OpSize::L,
                    };
                    push(
                        mf,
                        mb,
                        X86Opcode::Movd,
                        mv_size,
                        vec![X86Operand::VReg(vs)],
                        Some(X86Operand::VReg(tmp)),
                    );
                    match w {
                        IntWidth::I32 => push(
                            mf,
                            mb,
                            X86Opcode::Pshufd,
                            OpSize::Q,
                            vec![X86Operand::VReg(tmp), X86Operand::Imm(0)],
                            Some(X86Operand::VReg(dest)),
                        ),
                        IntWidth::I64 => push(
                            mf,
                            mb,
                            X86Opcode::Punpcklqdq,
                            OpSize::Q,
                            vec![X86Operand::VReg(tmp), X86Operand::VReg(tmp)],
                            Some(X86Operand::VReg(dest)),
                        ),
                        other => panic!("x10: packed broadcast of {:?} lanes", other),
                    }
                }
                other => panic!("x10: packed broadcast of {:?} should not reach isel", other),
            }
        }
        InstKind::VICmp(op, a, b) => {
            // i32 lanes only (the table refuses i64 compares on
            // SSE2). Eq/Gt are native; the rest are operand swaps
            // and/or an all-ones inversion.
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            emit_packed_icmp(mf, ctx, mb, *op, va, vb, dest);
        }
        InstKind::VFCmp(op, a, b) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let is_f32 = matches!(vec_elem_of(&inst.ty), IrType::Float(FloatWidth::F32));
            let cmp_op = if is_f32 {
                X86Opcode::Cmpps
            } else {
                X86Opcode::Cmppd
            };
            // cmpps predicates: 0=eq, 1=lt, 2=le, 4=neq. gt/ge swap
            // operands onto lt/le. Unordered (NaN) compares false for
            // the ordered predicates and true for neq — matching the
            // scalar ucomi+setcc lowering.
            let (imm, lhs, rhs) = match op {
                CmpOp::Eq => (0i64, va, vb),
                CmpOp::Lt => (1, va, vb),
                CmpOp::Le => (2, va, vb),
                CmpOp::Ne => (4, va, vb),
                CmpOp::Gt => (1, vb, va),
                CmpOp::Ge => (2, vb, va),
            };
            push(
                mf,
                mb,
                cmp_op,
                OpSize::Q,
                vec![
                    X86Operand::VReg(lhs),
                    X86Operand::VReg(rhs),
                    X86Operand::Imm(imm),
                ],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VSelect(mask, t, f) => {
            // (t & mask) | (~mask & f) via pand/pandn/por.
            let dest = ctx.lookup_vreg(inst.id);
            let vm = ctx.lookup_vreg(*mask);
            let vt = ctx.lookup_vreg(*t);
            let vf = ctx.lookup_vreg(*f);
            let t_and = mf.new_vreg(X86RegClass::Xmm128);
            push(
                mf,
                mb,
                X86Opcode::Pand,
                OpSize::Q,
                vec![X86Operand::VReg(vt), X86Operand::VReg(vm)],
                Some(X86Operand::VReg(t_and)),
            );
            // pandn computes ~dst & src with dst = tied operand 0,
            // so operand order is [mask, f].
            let f_andn = mf.new_vreg(X86RegClass::Xmm128);
            push(
                mf,
                mb,
                X86Opcode::Pandn,
                OpSize::Q,
                vec![X86Operand::VReg(vm), X86Operand::VReg(vf)],
                Some(X86Operand::VReg(f_andn)),
            );
            push(
                mf,
                mb,
                X86Opcode::Por,
                OpSize::Q,
                vec![X86Operand::VReg(t_and), X86Operand::VReg(f_andn)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::VReduceSum(v) | InstKind::VReduceMin(v) | InstKind::VReduceMax(v) => {
            let dest = ctx.lookup_vreg(inst.id);
            let vv = ctx.lookup_vreg(*v);
            let elem = func
                .value_type(*v)
                .map(|t| vec_elem_of(&t).clone())
                .unwrap_or_else(|| panic!("x10: missing vector type for reduce %{}", v.0));
            // The fold step is a single packed op for sum and float
            // min/max; signed i32 min/max has no native packed op at
            // SSE2, so its step is the pcmpgtd compare-and-blend
            // synthesis (x10c-1).
            let step = match (&inst.kind, &elem) {
                (InstKind::VReduceSum(_), IrType::Float(FloatWidth::F32)) => {
                    ReduceStep::Op(X86Opcode::Addps)
                }
                (InstKind::VReduceSum(_), IrType::Float(FloatWidth::F64)) => {
                    ReduceStep::Op(X86Opcode::Addpd)
                }
                (InstKind::VReduceSum(_), IrType::Int(IntWidth::I32)) => {
                    ReduceStep::Op(X86Opcode::Paddd)
                }
                (InstKind::VReduceSum(_), IrType::Int(IntWidth::I64)) => {
                    ReduceStep::Op(X86Opcode::Paddq)
                }
                (InstKind::VReduceMin(_), IrType::Float(FloatWidth::F32)) => {
                    ReduceStep::Op(X86Opcode::Minps)
                }
                (InstKind::VReduceMin(_), IrType::Float(FloatWidth::F64)) => {
                    ReduceStep::Op(X86Opcode::Minpd)
                }
                (InstKind::VReduceMax(_), IrType::Float(FloatWidth::F32)) => {
                    ReduceStep::Op(X86Opcode::Maxps)
                }
                (InstKind::VReduceMax(_), IrType::Float(FloatWidth::F64)) => {
                    ReduceStep::Op(X86Opcode::Maxpd)
                }
                (InstKind::VReduceMin(_), IrType::Int(IntWidth::I32)) => ReduceStep::IntMinMax(true),
                (InstKind::VReduceMax(_), IrType::Int(IntWidth::I32)) => ReduceStep::IntMinMax(false),
                (k, e) => panic!("x10: reduce {:?} on {:?} should not reach isel", k, e),
            };
            // Fold the upper half onto the lower, then (for 4-lane
            // shapes) lane 1 onto lane 0, all in packed registers;
            // lane 0 is the answer.
            let half = mf.new_vreg(X86RegClass::Xmm128);
            push(
                mf,
                mb,
                X86Opcode::Pshufd,
                OpSize::Q,
                vec![X86Operand::VReg(vv), X86Operand::Imm(0x4E)],
                Some(X86Operand::VReg(half)),
            );
            let fold1 = mf.new_vreg(X86RegClass::Xmm128);
            emit_reduce_step(mf, mb, step, vv, half, fold1);
            let four_lanes = matches!(
                elem,
                IrType::Float(FloatWidth::F32) | IrType::Int(IntWidth::I32)
            );
            let final_vec = if four_lanes {
                let lane1 = mf.new_vreg(X86RegClass::Xmm128);
                push(
                    mf,
                    mb,
                    X86Opcode::Pshufd,
                    OpSize::Q,
                    vec![X86Operand::VReg(fold1), X86Operand::Imm(0xE5)],
                    Some(X86Operand::VReg(lane1)),
                );
                let fold0 = mf.new_vreg(X86RegClass::Xmm128);
                emit_reduce_step(mf, mb, step, fold1, lane1, fold0);
                fold0
            } else {
                fold1
            };
            // Extract lane 0 into the scalar destination.
            match elem {
                IrType::Float(FloatWidth::F32) => push(
                    mf,
                    mb,
                    X86Opcode::Movss,
                    OpSize::L,
                    vec![X86Operand::VReg(final_vec)],
                    Some(X86Operand::VReg(dest)),
                ),
                IrType::Float(FloatWidth::F64) => push(
                    mf,
                    mb,
                    X86Opcode::Movsd,
                    OpSize::Q,
                    vec![X86Operand::VReg(final_vec)],
                    Some(X86Operand::VReg(dest)),
                ),
                IrType::Int(w) => push(
                    mf,
                    mb,
                    X86Opcode::Movd,
                    match w {
                        IntWidth::I64 => OpSize::Q,
                        _ => OpSize::L,
                    },
                    vec![X86Operand::VReg(final_vec)],
                    Some(X86Operand::VReg(dest)),
                ),
                other => panic!("x10: reduce extract for {:?}", other),
            }
        }
        InstKind::FPow(a, b) => {
            // libm call, mirroring ARM isel's pow/powf lowering. Two
            // xmm argument registers → AL=2 per the blanket external
            // policy.
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let width = float_width(&inst.ty);
            let callee = match width {
                FloatWidth::F32 => "powf",
                FloatWidth::F64 => "pow",
            };
            let (mv, sz) = (fp_move_w(width), fp_size_w(width));
            push(
                mf,
                mb,
                mv,
                sz,
                vec![X86Operand::VReg(va)],
                Some(X86Operand::Reg(X86Reg::Xmm0)),
            );
            push(
                mf,
                mb,
                mv,
                sz,
                vec![X86Operand::VReg(vb)],
                Some(X86Operand::Reg(X86Reg::Xmm1)),
            );
            push(
                mf,
                mb,
                X86Opcode::MovRI,
                OpSize::B,
                vec![X86Operand::Imm(2)],
                Some(X86Operand::Reg(X86Reg::Rax)),
            );
            push(
                mf,
                mb,
                X86Opcode::Call,
                OpSize::Q,
                vec![X86Operand::Extern(callee.into())],
                None,
            );
            push(
                mf,
                mb,
                mv,
                sz,
                vec![X86Operand::Reg(X86Reg::Xmm0)],
                Some(X86Operand::VReg(dest)),
            );
        }

        // ---- Comparisons ----
        InstKind::ICmp(op, a, b) => {
            let dest = ctx.lookup_vreg(inst.id);
            let a_i128 = matches!(func.value_type(*a), Some(IrType::Int(IntWidth::I128)));
            let b_i128 = matches!(func.value_type(*b), Some(IrType::Int(IntWidth::I128)));
            if a_i128 || b_i128 {
                assert!(
                    a_i128 && b_i128,
                    "isel: mixed-width i128 compare should have been widened in IR"
                );
                emit_i128_icmp(mf, ctx, mb, *op, *a, *b, dest);
                return mb;
            }
            let wide = needs_wide_icmp(func.value_type(*a).as_ref())
                || needs_wide_icmp(func.value_type(*b).as_ref());
            let va = cmp_operand_vreg(mf, ctx, mb, func, *a, wide);
            let vb = cmp_operand_vreg(mf, ctx, mb, func, *b, wide);
            let size = if wide { OpSize::Q } else { OpSize::L };
            push(
                mf,
                mb,
                X86Opcode::Cmp,
                size,
                vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
                None,
            );
            emit_setcc_movzx(mf, mb, int_cond(*op), dest);
        }

        InstKind::FCmp(op, a, b) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let operand_ty = func
                .value_type(*a)
                .unwrap_or_else(|| panic!("isel: missing type for fcmp operand %{}", a.0));
            let width = float_width(&operand_ty);
            let ucomi = match width {
                FloatWidth::F32 => X86Opcode::Ucomiss,
                FloatWidth::F64 => X86Opcode::Ucomisd,
            };
            let size = fp_size_w(width);
            match fp_cond_plan(*op) {
                FpCmpPlan::Direct {
                    swap_operands,
                    cond,
                } => {
                    let (l, r) = if swap_operands { (vb, va) } else { (va, vb) };
                    push(
                        mf,
                        mb,
                        ucomi,
                        size,
                        vec![X86Operand::VReg(l), X86Operand::VReg(r)],
                        None,
                    );
                    emit_setcc_movzx(mf, mb, cond, dest);
                }
                FpCmpPlan::Equality {
                    cond,
                    parity,
                    combine,
                } => {
                    push(
                        mf,
                        mb,
                        ucomi,
                        size,
                        vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
                        None,
                    );
                    // Two setcc reads of the same flags (setcc writes
                    // no flags), combined at byte width, then widened.
                    let t1 = mf.new_vreg(X86RegClass::Gp8);
                    let t2 = mf.new_vreg(X86RegClass::Gp8);
                    let t3 = mf.new_vreg(X86RegClass::Gp8);
                    push(
                        mf,
                        mb,
                        X86Opcode::Setcc,
                        OpSize::B,
                        vec![X86Operand::Cond(cond)],
                        Some(X86Operand::VReg(t1)),
                    );
                    push(
                        mf,
                        mb,
                        X86Opcode::Setcc,
                        OpSize::B,
                        vec![X86Operand::Cond(parity)],
                        Some(X86Operand::VReg(t2)),
                    );
                    push(
                        mf,
                        mb,
                        combine,
                        OpSize::B,
                        vec![X86Operand::VReg(t1), X86Operand::VReg(t2)],
                        Some(X86Operand::VReg(t3)),
                    );
                    push(
                        mf,
                        mb,
                        X86Opcode::Movzx { src: OpSize::B },
                        OpSize::L,
                        vec![X86Operand::VReg(t3)],
                        Some(X86Operand::VReg(dest)),
                    );
                }
            }
        }

        // ---- Logic (Bool values are materialized 0/1 in Gp32) ----
        InstKind::And(a, b) | InstKind::BitAnd(a, b) => {
            emit_int_binop(mf, ctx, mb, inst, X86Opcode::And, *a, *b)
        }
        InstKind::Or(a, b) | InstKind::BitOr(a, b) => {
            emit_int_binop(mf, ctx, mb, inst, X86Opcode::Or, *a, *b)
        }
        InstKind::BitXor(a, b) => emit_int_binop(mf, ctx, mb, inst, X86Opcode::Xor, *a, *b),
        InstKind::Not(a) => {
            // Logical not of a 0/1 bool: xor with 1 flips exactly the
            // value bit (a full `not` would set the unused high bits).
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            push(
                mf,
                mb,
                X86Opcode::Xor,
                OpSize::L,
                vec![X86Operand::VReg(va), X86Operand::Imm(1)],
                Some(X86Operand::VReg(dest)),
            );
        }
        InstKind::BitNot(a) => {
            let dest = ctx.lookup_vreg(inst.id);
            let va = ctx.lookup_vreg(*a);
            push(
                mf,
                mb,
                X86Opcode::Not,
                op_size(&inst.ty),
                vec![X86Operand::VReg(va)],
                Some(X86Operand::VReg(dest)),
            );
        }

        // ---- Shifts (variable count through %cl) ----
        InstKind::Shl(a, b) => emit_shift(mf, ctx, mb, inst, X86Opcode::Shl, *a, *b),
        InstKind::LShr(a, b) => emit_shift(mf, ctx, mb, inst, X86Opcode::Shr, *a, *b),
        InstKind::AShr(a, b) => emit_shift(mf, ctx, mb, inst, X86Opcode::Sar, *a, *b),

        InstKind::CountLeadingZeros(_)
        | InstKind::CountTrailingZeros(_)
        | InstKind::PopCount(_) => {
            panic!("x05 scope: bit-count ops deferred (no lzcnt/tzcnt/popcnt opcodes yet)")
        }

        // ---- Select: simple diamond over new machine blocks ----
        InstKind::Select(cond, tv, fv) => {
            let dest = ctx.lookup_vreg(inst.id);
            let vcond = ctx.lookup_vreg(*cond);
            let vt = ctx.lookup_vreg(*tv);
            let vf = ctx.lookup_vreg(*fv);
            let false_mb = mf.new_block();
            let join_mb = mf.new_block();
            push(
                mf,
                mb,
                X86Opcode::Test,
                OpSize::L,
                vec![X86Operand::VReg(vcond), X86Operand::VReg(vcond)],
                None,
            );
            push(
                mf,
                mb,
                X86Opcode::Jcc,
                OpSize::Q,
                vec![X86Operand::Cond(X86Cond::E), X86Operand::BlockRef(false_mb)],
                None,
            );
            // True arm stays inline; both arms end in explicit jumps so
            // physical block order never matters.
            emit_vreg_move(mf, mb, dest, vt);
            push(
                mf,
                mb,
                X86Opcode::Jmp,
                OpSize::Q,
                vec![X86Operand::BlockRef(join_mb)],
                None,
            );
            emit_vreg_move(mf, false_mb, dest, vf);
            push(
                mf,
                false_mb,
                X86Opcode::Jmp,
                OpSize::Q,
                vec![X86Operand::BlockRef(join_mb)],
                None,
            );
            return join_mb;
        }

        // ---- Conversions ----
        InstKind::IntToFloat(a, fw) => {
            let dest = ctx.lookup_vreg(inst.id);
            let src_ty = func.value_type(*a);
            let mut src = ctx.lookup_vreg(*a);
            // cvtsi2 has no 8/16-bit source forms; sign-extend narrow
            // ints to 32 bits first. Bool is already a zero-extended
            // 0/1 in its Gp32 vreg.
            let isz = match src_ty.as_ref() {
                Some(IrType::Int(IntWidth::I64)) => OpSize::Q,
                Some(IrType::Int(IntWidth::I8)) => {
                    src = emit_movsx_to_gp32(mf, mb, src, OpSize::B);
                    OpSize::L
                }
                Some(IrType::Int(IntWidth::I16)) => {
                    src = emit_movsx_to_gp32(mf, mb, src, OpSize::W);
                    OpSize::L
                }
                _ => OpSize::L,
            };
            let opcode = match fw {
                FloatWidth::F32 => X86Opcode::Cvtsi2ss,
                FloatWidth::F64 => X86Opcode::Cvtsi2sd,
            };
            push(
                mf,
                mb,
                opcode,
                isz,
                vec![X86Operand::VReg(src)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::FloatToInt(a, iw) => {
            let dest = ctx.lookup_vreg(inst.id);
            let src = ctx.lookup_vreg(*a);
            let src_ty = func
                .value_type(*a)
                .unwrap_or_else(|| panic!("isel: missing type for float-to-int operand"));
            let opcode = match float_width(&src_ty) {
                FloatWidth::F32 => X86Opcode::Cvttss2si,
                FloatWidth::F64 => X86Opcode::Cvttsd2si,
            };
            // Narrow targets truncate from the 32-bit conversion.
            let size = match iw {
                IntWidth::I64 => OpSize::Q,
                _ => OpSize::L,
            };
            push(
                mf,
                mb,
                opcode,
                size,
                vec![X86Operand::VReg(src)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::FloatExtend(a, _) => {
            let dest = ctx.lookup_vreg(inst.id);
            let src = ctx.lookup_vreg(*a);
            push(
                mf,
                mb,
                X86Opcode::Cvtss2sd,
                OpSize::Q,
                vec![X86Operand::VReg(src)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::FloatTrunc(a, _) => {
            let dest = ctx.lookup_vreg(inst.id);
            let src = ctx.lookup_vreg(*a);
            push(
                mf,
                mb,
                X86Opcode::Cvtsd2ss,
                OpSize::Q,
                vec![X86Operand::VReg(src)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::IntExtend(a, _, signed) => {
            let dest = ctx.lookup_vreg(inst.id);
            let src = ctx.lookup_vreg(*a);
            let src_ty = func.value_type(*a);
            let src_bits = match src_ty.as_ref() {
                Some(IrType::Int(IntWidth::I8)) => 8,
                Some(IrType::Int(IntWidth::I16)) => 16,
                Some(IrType::Int(IntWidth::I64)) => 64,
                _ => 32, // i32, Bool, conservative default
            };
            let dst_bits = if dest.class == X86RegClass::Gp64 {
                64
            } else {
                32
            };
            let dst_size = if dst_bits == 64 { OpSize::Q } else { OpSize::L };
            if !*signed || src_bits >= dst_bits {
                match src_bits {
                    8 if src_bits < dst_bits || !*signed => {
                        // movzbl zero-extends through bit 63.
                        push(
                            mf,
                            mb,
                            X86Opcode::Movzx { src: OpSize::B },
                            OpSize::L,
                            vec![X86Operand::VReg(src)],
                            Some(X86Operand::VReg(dest)),
                        );
                    }
                    16 if src_bits < dst_bits || !*signed => {
                        push(
                            mf,
                            mb,
                            X86Opcode::Movzx { src: OpSize::W },
                            OpSize::L,
                            vec![X86Operand::VReg(src)],
                            Some(X86Operand::VReg(dest)),
                        );
                    }
                    _ => {
                        // 32→64 zero extension is `movl` (implicit
                        // upper-half zeroing, the printer's documented
                        // rule); same-width/wider sources degrade to a
                        // plain move like ARM isel does. A 64→64
                        // "extend" must move the full quad: a movl here
                        // writes only 4 bytes of the def's 8-byte spill
                        // slot and the reload picks up stale upper bytes
                        // (x09: popcnt(int64) counted garbage bits).
                        let mov_size = if src_bits == 64 && dst_bits == 64 {
                            OpSize::Q
                        } else {
                            OpSize::L
                        };
                        push(
                            mf,
                            mb,
                            X86Opcode::MovRR,
                            mov_size,
                            vec![X86Operand::VReg(src)],
                            Some(X86Operand::VReg(dest)),
                        );
                    }
                }
            } else {
                let src_size = match src_bits {
                    8 => OpSize::B,
                    16 => OpSize::W,
                    _ => OpSize::L,
                };
                push(
                    mf,
                    mb,
                    X86Opcode::Movsx { src: src_size },
                    dst_size,
                    vec![X86Operand::VReg(src)],
                    Some(X86Operand::VReg(dest)),
                );
            }
        }

        InstKind::IntTrunc(a, _) => {
            // A 32-bit move truncates and keeps the value's low bits;
            // narrow ints live in Gp32 anyway.
            let dest = ctx.lookup_vreg(inst.id);
            let src = ctx.lookup_vreg(*a);
            push(
                mf,
                mb,
                X86Opcode::MovRR,
                OpSize::L,
                vec![X86Operand::VReg(src)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::PtrToInt(a) | InstKind::IntToPtr(a, _) => {
            let dest = ctx.lookup_vreg(inst.id);
            let src = ctx.lookup_vreg(*a);
            push(
                mf,
                mb,
                X86Opcode::MovRR,
                OpSize::Q,
                vec![X86Operand::VReg(src)],
                Some(X86Operand::VReg(dest)),
            );
        }

        // ---- Memory ----
        InstKind::Alloca(_) => {
            // Phase 1 reserved the slot; the SSA value is its address.
            let slot = ctx.alloca_slots[&inst.id];
            let dest = ctx.lookup_vreg(inst.id);
            push(
                mf,
                mb,
                X86Opcode::Lea,
                OpSize::Q,
                vec![X86Operand::FrameSlot(slot)],
                Some(X86Operand::VReg(dest)),
            );
        }

        InstKind::Load(addr) => {
            let dest = ctx.lookup_vreg(inst.id);
            let addr_op = ctx.addr_operand(*addr);
            emit_load(mf, mb, dest, &inst.ty, addr_op);
        }

        InstKind::Store(val, addr) => {
            let val_ty = func
                .value_type(*val)
                .unwrap_or_else(|| panic!("isel: missing type for stored value %{}", val.0));
            if is_wide_pair_ty(&val_ty) {
                let src = ctx.lookup_wide_slot(*val);
                emit_load_slot_pair(mf, mb, src, X86Reg::Rax, X86Reg::Rdx);
                match ctx.addr_operand(*addr) {
                    X86Operand::FrameSlot(base_slot) => {
                        push(
                            mf,
                            mb,
                            X86Opcode::Lea,
                            OpSize::Q,
                            vec![X86Operand::FrameSlot(base_slot)],
                            Some(X86Operand::Reg(X86Reg::Rcx)),
                        );
                    }
                    vreg_op => {
                        push(
                            mf,
                            mb,
                            X86Opcode::MovRR,
                            OpSize::Q,
                            vec![vreg_op],
                            Some(X86Operand::Reg(X86Reg::Rcx)),
                        );
                    }
                }
                for (disp, reg) in [(0i64, X86Reg::Rax), (8, X86Reg::Rdx)] {
                    push(
                        mf,
                        mb,
                        X86Opcode::MovMR,
                        OpSize::Q,
                        vec![
                            X86Operand::Reg(reg),
                            X86Operand::Mem {
                                base: Some(X86Reg::Rcx),
                                index: None,
                                scale: 1,
                                disp,
                            },
                        ],
                        None,
                    );
                }
                return mb;
            }
            let src = ctx.lookup_vreg(*val);
            let addr_op = ctx.addr_operand(*addr);
            emit_store(mf, mb, src, &val_ty, addr_op);
        }

        InstKind::GetElementPtr(base, indices) => {
            let dest = ctx.lookup_vreg(inst.id);
            let base_v = ctx.lookup_vreg(*base);
            debug_assert_eq!(
                base_v.class,
                X86RegClass::Gp64,
                "GEP base must be a pointer"
            );
            // Element size from the result type Ptr<elem> — same
            // derivation ARM isel uses.
            let elem_size = match &inst.ty {
                IrType::Ptr(inner) => match inner.as_ref() {
                    IrType::Struct(_) => alloca_size(inner, ctx.layout) as i64,
                    _ => inner.size_bytes(&ctx.layout) as i64,
                },
                _ => 4,
            };
            if let Some(idx) = indices.first() {
                let idx_src = ctx.lookup_vreg(*idx);
                let idx64 = if idx_src.class == X86RegClass::Gp64 {
                    idx_src
                } else {
                    let widened = mf.new_vreg(X86RegClass::Gp64);
                    let (opcode, size) = if matches!(func.value_type(*idx), Some(IrType::Bool)) {
                        (X86Opcode::MovRR, OpSize::L) // zero-extend
                    } else {
                        (X86Opcode::Movsx { src: OpSize::L }, OpSize::Q)
                    };
                    push(
                        mf,
                        mb,
                        opcode,
                        size,
                        vec![X86Operand::VReg(idx_src)],
                        Some(X86Operand::VReg(widened)),
                    );
                    widened
                };
                // Byte offset = index * elem_size via plain imul
                // (always — lea/shift strength reduction is x09 work),
                // then add to the base. Both ops are three-address
                // here; twoaddr satisfies the ties.
                let size_v = mf.new_vreg(X86RegClass::Gp64);
                push(
                    mf,
                    mb,
                    X86Opcode::MovRI,
                    OpSize::Q,
                    vec![X86Operand::Imm(elem_size)],
                    Some(X86Operand::VReg(size_v)),
                );
                let scaled = mf.new_vreg(X86RegClass::Gp64);
                push(
                    mf,
                    mb,
                    X86Opcode::Imul,
                    OpSize::Q,
                    vec![X86Operand::VReg(idx64), X86Operand::VReg(size_v)],
                    Some(X86Operand::VReg(scaled)),
                );
                push(
                    mf,
                    mb,
                    X86Opcode::Add,
                    OpSize::Q,
                    vec![X86Operand::VReg(base_v), X86Operand::VReg(scaled)],
                    Some(X86Operand::VReg(dest)),
                );
            } else {
                push(
                    mf,
                    mb,
                    X86Opcode::MovRR,
                    OpSize::Q,
                    vec![X86Operand::VReg(base_v)],
                    Some(X86Operand::VReg(dest)),
                );
            }
        }

        InstKind::GlobalAddr(name) => {
            let dest = ctx.lookup_vreg(inst.id);
            // Always rip-relative: works in every code model and is
            // what x06's link step expects.
            push(
                mf,
                mb,
                X86Opcode::Lea,
                OpSize::Q,
                vec![X86Operand::RipLabel(name.clone())],
                Some(X86Operand::VReg(dest)),
            );
        }

        // ---- Calls ----
        InstKind::Call(..) | InstKind::RuntimeCall(..) => {
            select_call_inst(mf, ctx, mb, inst, func, func_names);
        }

        InstKind::ExtractField(..) | InstKind::InsertField(..) => {
            panic!("x05 scope: aggregate field ops deferred")
        }

        // VBitcast/VExtract/VInsert: no producer emits them today
        // (the vectorizer's surface is selected above); reject loudly
        // rather than guess at lane semantics.
        InstKind::VBitcast(..) | InstKind::VExtract(..) | InstKind::VInsert(..) => {
            panic!("x10 scope: vector lane ops VBitcast/VExtract/VInsert have no producer")
        }
    }
    mb
}

/// Three-address integer binop; twoaddr satisfies the tie later.
fn emit_int_binop(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    opcode: X86Opcode,
    a: ValueId,
    b: ValueId,
) {
    let dest = ctx.lookup_vreg(inst.id);
    let va = ctx.lookup_vreg(a);
    let vb = ctx.lookup_vreg(b);
    push(
        mf,
        mb,
        opcode,
        op_size(&inst.ty),
        vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
        Some(X86Operand::VReg(dest)),
    );
}

/// `ops` is the (ss, sd) opcode pair; the result width picks one.
fn emit_fp_binop(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    a: ValueId,
    b: ValueId,
    ops: (X86Opcode, X86Opcode),
) {
    let dest = ctx.lookup_vreg(inst.id);
    let va = ctx.lookup_vreg(a);
    let vb = ctx.lookup_vreg(b);
    let opcode = match float_width(&inst.ty) {
        FloatWidth::F32 => ops.0,
        FloatWidth::F64 => ops.1,
    };
    push(
        mf,
        mb,
        opcode,
        fp_size(&inst.ty),
        vec![X86Operand::VReg(va), X86Operand::VReg(vb)],
        Some(X86Operand::VReg(dest)),
    );
}

/// FNeg/FAbs: packed bit op against a rodata mask loaded into a fresh
/// Xmm vreg (see the call sites for the lane-zeroing argument).
fn emit_fp_mask_op(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    a: ValueId,
    mask_bits: u64,
    opcode: X86Opcode,
) {
    let dest = ctx.lookup_vreg(inst.id);
    let va = ctx.lookup_vreg(a);
    let width = float_width(&inst.ty);
    let label = mf.add_rodata(mask_bits);
    let mask_v = mf.new_vreg(X86RegClass::Xmm);
    push(
        mf,
        mb,
        fp_move_w(width),
        fp_size_w(width),
        vec![X86Operand::RipLabel(label)],
        Some(X86Operand::VReg(mask_v)),
    );
    push(
        mf,
        mb,
        opcode,
        fp_size_w(width),
        vec![X86Operand::VReg(va), X86Operand::VReg(mask_v)],
        Some(X86Operand::VReg(dest)),
    );
}

/// IDiv/IMod fixed-register sequence. `result_reg` is Rax (quotient)
/// or Rdx (remainder).
fn emit_div_mod(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    a: ValueId,
    b: ValueId,
    result_reg: X86Reg,
) {
    let dest = ctx.lookup_vreg(inst.id);
    let va = ctx.lookup_vreg(a);
    let vb = ctx.lookup_vreg(b);
    let size = op_size(&inst.ty);
    // The divisor must end up in neither rax nor rdx (both are
    // clobbered by cltd/cqto + idiv). Always copy it to a fresh vreg
    // whose interval crosses the clobbers, so the allocator keeps it
    // clear of them.
    let divisor = mf.new_vreg(dest.class);
    push(
        mf,
        mb,
        X86Opcode::MovRR,
        size,
        vec![X86Operand::VReg(vb)],
        Some(X86Operand::VReg(divisor)),
    );
    push(
        mf,
        mb,
        X86Opcode::MovRR,
        size,
        vec![X86Operand::VReg(va)],
        Some(X86Operand::Reg(X86Reg::Rax)),
    );
    let extend = match size {
        OpSize::Q => X86Opcode::Cqto,
        _ => X86Opcode::Cltd,
    };
    push(mf, mb, extend, size, vec![], None);
    push(
        mf,
        mb,
        X86Opcode::Idiv,
        size,
        vec![X86Operand::VReg(divisor)],
        None,
    );
    push(
        mf,
        mb,
        X86Opcode::MovRR,
        size,
        vec![X86Operand::Reg(result_reg)],
        Some(X86Operand::VReg(dest)),
    );
}

/// Variable shift: count through %rcx (printed as %cl).
fn emit_shift(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    opcode: X86Opcode,
    a: ValueId,
    b: ValueId,
) {
    let dest = ctx.lookup_vreg(inst.id);
    let va = ctx.lookup_vreg(a);
    let vb = ctx.lookup_vreg(b);
    push(
        mf,
        mb,
        X86Opcode::MovRR,
        gp_move_size(vb.class),
        vec![X86Operand::VReg(vb)],
        Some(X86Operand::Reg(X86Reg::Rcx)),
    );
    push(
        mf,
        mb,
        opcode,
        op_size(&inst.ty),
        vec![X86Operand::VReg(va), X86Operand::Reg(X86Reg::Rcx)],
        Some(X86Operand::VReg(dest)),
    );
}

/// setcc into a fresh Gp8 vreg + movzx to the Gp32 dest. ALWAYS
/// paired: setcc writes 8 bits and leaves bits 8–63 stale.
fn emit_setcc_movzx(mf: &mut X86Function, mb: MBlockId, cond: X86Cond, dest: X86VReg) {
    let t = mf.new_vreg(X86RegClass::Gp8);
    push(
        mf,
        mb,
        X86Opcode::Setcc,
        OpSize::B,
        vec![X86Operand::Cond(cond)],
        Some(X86Operand::VReg(t)),
    );
    push(
        mf,
        mb,
        X86Opcode::Movzx { src: OpSize::B },
        OpSize::L,
        vec![X86Operand::VReg(t)],
        Some(X86Operand::VReg(dest)),
    );
}

fn emit_movsx_to_gp32(mf: &mut X86Function, mb: MBlockId, src: X86VReg, from: OpSize) -> X86VReg {
    let dest = mf.new_vreg(X86RegClass::Gp32);
    push(
        mf,
        mb,
        X86Opcode::Movsx { src: from },
        OpSize::L,
        vec![X86Operand::VReg(src)],
        Some(X86Operand::VReg(dest)),
    );
    dest
}

fn needs_wide_icmp(ty: Option<&IrType>) -> bool {
    matches!(
        ty,
        Some(IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_))
    )
}

/// Widen one icmp operand to 64 bits when the comparison is wide.
fn cmp_operand_vreg(
    mf: &mut X86Function,
    ctx: &X86ISelCtx,
    mb: MBlockId,
    func: &Function,
    value: ValueId,
    wide: bool,
) -> X86VReg {
    let src = ctx.lookup_vreg(value);
    if !wide || src.class == X86RegClass::Gp64 {
        return src;
    }
    let dest = mf.new_vreg(X86RegClass::Gp64);
    let (opcode, size) = if matches!(func.value_type(value), Some(IrType::Bool)) {
        (X86Opcode::MovRR, OpSize::L) // 32→64 zero extension
    } else {
        (X86Opcode::Movsx { src: OpSize::L }, OpSize::Q)
    };
    push(
        mf,
        mb,
        opcode,
        size,
        vec![X86Operand::VReg(src)],
        Some(X86Operand::VReg(dest)),
    );
    dest
}

/// Register-to-register move dispatched by the destination's class.
/// Xmm copies use movsd (low 64 bits) regardless of scalar width —
/// the upper bits of an f32 scalar vreg are never read.
fn emit_vreg_move(mf: &mut X86Function, mb: MBlockId, dst: X86VReg, src: X86VReg) {
    let (opcode, size) = match dst.class {
        X86RegClass::Xmm => (X86Opcode::Movsd, OpSize::Q),
        // Vector block params (the vectorized accumulator) move all
        // 128 bits (x10).
        X86RegClass::Xmm128 => (X86Opcode::Movaps, OpSize::Q),
        class => (X86Opcode::MovRR, gp_move_size(class)),
    };
    push(
        mf,
        mb,
        opcode,
        size,
        vec![X86Operand::VReg(src)],
        Some(X86Operand::VReg(dst)),
    );
}

/// Typed load into a vreg. Narrow integers extend to the Gp32
/// convention width (sign for i8/i16, zero for Bool).
fn emit_load(mf: &mut X86Function, mb: MBlockId, dest: X86VReg, ty: &IrType, addr: X86Operand) {
    let (opcode, size) = match ty {
        IrType::Bool => (X86Opcode::MovzxRM { src: OpSize::B }, OpSize::L),
        IrType::Int(IntWidth::I8) => (X86Opcode::MovsxRM { src: OpSize::B }, OpSize::L),
        IrType::Int(IntWidth::I16) => (X86Opcode::MovsxRM { src: OpSize::W }, OpSize::L),
        IrType::Int(IntWidth::I32) => (X86Opcode::MovRM, OpSize::L),
        IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_) => {
            (X86Opcode::MovRM, OpSize::Q)
        }
        IrType::Float(FloatWidth::F32) => (X86Opcode::Movss, OpSize::L),
        IrType::Float(FloatWidth::F64) => (X86Opcode::Movsd, OpSize::Q),
        IrType::Array(elem, n) if scalar_elem_bytes(elem) * n == 8 => (X86Opcode::MovRM, OpSize::Q),
        other => panic!("x05 scope: load of {:?} deferred", other),
    };
    push(
        mf,
        mb,
        opcode,
        size,
        vec![addr],
        Some(X86Operand::VReg(dest)),
    );
}

/// Typed store. Integer stores are `MovMR [value, addr]` with no def;
/// FP stores reuse Movss/Movsd with the address as the def operand
/// (the printer's `movss %src, dst` shape — `mov{l,q}` can't move
/// from an xmm register).
fn emit_store(mf: &mut X86Function, mb: MBlockId, src: X86VReg, ty: &IrType, addr: X86Operand) {
    match ty {
        IrType::Bool | IrType::Int(IntWidth::I8) => push(
            mf,
            mb,
            X86Opcode::MovMR,
            OpSize::B,
            vec![X86Operand::VReg(src), addr],
            None,
        ),
        IrType::Int(IntWidth::I16) => push(
            mf,
            mb,
            X86Opcode::MovMR,
            OpSize::W,
            vec![X86Operand::VReg(src), addr],
            None,
        ),
        IrType::Int(IntWidth::I32) => push(
            mf,
            mb,
            X86Opcode::MovMR,
            OpSize::L,
            vec![X86Operand::VReg(src), addr],
            None,
        ),
        IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_) => push(
            mf,
            mb,
            X86Opcode::MovMR,
            OpSize::Q,
            vec![X86Operand::VReg(src), addr],
            None,
        ),
        IrType::Float(w) => push(
            mf,
            mb,
            fp_move_w(*w),
            fp_size_w(*w),
            vec![X86Operand::VReg(src)],
            Some(addr),
        ),
        IrType::Array(elem, n) if scalar_elem_bytes(elem) * n == 8 => push(
            mf,
            mb,
            X86Opcode::MovMR,
            OpSize::Q,
            vec![X86Operand::VReg(src), addr],
            None,
        ),
        other => panic!("x05 scope: store of {:?} deferred", other),
    }
}

fn select_call_inst(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    func: &Function,
    func_names: &[String],
) {
    // Indirect targets: the pointer value is staged into r11 BEFORE
    // argument marshaling could clobber anything (r11 is caller-saved,
    // never an argument register, and distinct from rax, which the AL
    // varargs rule writes immediately before the call).
    let mut indirect_target: Option<X86VReg> = None;
    let (label, args, is_internal) = match &inst.kind {
        InstKind::Call(FuncRef::External(name), args) => (name.clone(), args.as_slice(), false),
        InstKind::Call(FuncRef::Internal(idx), args) => {
            let name = func_names
                .get(*idx as usize)
                .unwrap_or_else(|| panic!("isel: internal func index {} out of range", idx))
                .clone();
            (name, args.as_slice(), true)
        }
        InstKind::Call(FuncRef::Indirect(target), args) => {
            indirect_target = Some(ctx.lookup_vreg(*target));
            (String::new(), args.as_slice(), false)
        }
        InstKind::RuntimeCall(rf, args) => {
            (runtime_func_symbol(rf, args, func), args.as_slice(), false)
        }
        _ => unreachable!(),
    };

    let mut state = X86AbiState::new();
    let mut arg_locs: Vec<(ValueId, X86AbiArgLoc, IrType)> = Vec::with_capacity(args.len());
    for &arg in args {
        let ty = func
            .value_type(arg)
            .unwrap_or_else(|| panic!("isel: missing type for call arg %{}", arg.0));
        let loc = classify_abi_arg(&abi_ty_of(&ty), &mut state);
        arg_locs.push((arg, loc, ty));
    }
    if state.stack_bytes() > 0 {
        mf.reserve_outgoing_args(state.stack_bytes());
    }

    // Stack stores go out first; register moves are batched last so no
    // later argument computation can clobber an argument register.
    let mut pending: Vec<X86Inst> = Vec::new();
    for (arg, loc, ty) in &arg_locs {
        if matches!(ty, IrType::Array(..)) {
            match (loc, is_wide_pair_ty(ty)) {
                // complex(4): one xmm, packed (movq raw bits across).
                (X86AbiArgLoc::Xmm(r), false) => {
                    let v = ctx.lookup_vreg(*arg);
                    pending.push(X86Inst {
                        opcode: X86Opcode::MovqGpToXmm,
                        size: OpSize::Q,
                        operands: vec![X86Operand::VReg(v)],
                        def: Some(X86Operand::Reg(*r)),
                    });
                }
                // complex(8): two xmm regs from the slot pair.
                (X86AbiArgLoc::XmmPair(a, b), true) => {
                    let slots = ctx.lookup_wide_slot(*arg);
                    for (slot, xmm) in [(slots.0, *a), (slots.1, *b)] {
                        pending.push(X86Inst {
                            opcode: X86Opcode::Movsd,
                            size: OpSize::Q,
                            operands: vec![X86Operand::FrameSlot(slot)],
                            def: Some(X86Operand::Reg(xmm)),
                        });
                    }
                }
                (X86AbiArgLoc::Stack { offset, .. }, wide) => {
                    if wide {
                        let slots = ctx.lookup_wide_slot(*arg);
                        emit_load_slot_pair(mf, mb, slots, X86Reg::Rax, X86Reg::Rdx);
                        for (half, reg) in [(0i64, X86Reg::Rax), (8, X86Reg::Rdx)] {
                            push(
                                mf,
                                mb,
                                X86Opcode::MovMR,
                                OpSize::Q,
                                vec![
                                    X86Operand::Reg(reg),
                                    X86Operand::Mem {
                                        base: Some(X86Reg::Rsp),
                                        index: None,
                                        scale: 1,
                                        disp: offset + half,
                                    },
                                ],
                                None,
                            );
                        }
                    } else {
                        let v = ctx.lookup_vreg(*arg);
                        push(
                            mf,
                            mb,
                            X86Opcode::MovMR,
                            OpSize::Q,
                            vec![
                                X86Operand::VReg(v),
                                X86Operand::Mem {
                                    base: Some(X86Reg::Rsp),
                                    index: None,
                                    scale: 1,
                                    disp: *offset,
                                },
                            ],
                            None,
                        );
                    }
                }
                (other, wide) => panic!(
                    "isel: unexpected ABI loc {:?} for complex arg (wide={})",
                    other, wide
                ),
            }
            continue;
        }
        if matches!(ty, IrType::Int(IntWidth::I128)) {
            let slots = ctx.lookup_wide_slot(*arg);
            match loc {
                // psABI: a two-eightbyte INTEGER argument takes two
                // consecutive GP argument registers (abi.tex §3.2.3);
                // when fewer than two remain, the WHOLE argument goes
                // to the stack and the registers stay available for
                // later args (the classifier owns that revert rule).
                X86AbiArgLoc::GpPair(lo, hi) => {
                    // Direct slot→argreg loads; deferring to `pending`
                    // is unnecessary because the sources are frame
                    // slots, which later arg computation cannot
                    // clobber... but a later arg's own staging COULD
                    // overwrite lo/hi if they alias rax/rdx scratches
                    // used by i128 stack stores below. Batch with
                    // `pending` like every other register arg.
                    pending.push(X86Inst {
                        opcode: X86Opcode::MovRM,
                        size: OpSize::Q,
                        operands: vec![X86Operand::FrameSlot(slots.0)],
                        def: Some(X86Operand::Reg(*lo)),
                    });
                    pending.push(X86Inst {
                        opcode: X86Opcode::MovRM,
                        size: OpSize::Q,
                        operands: vec![X86Operand::FrameSlot(slots.1)],
                        def: Some(X86Operand::Reg(*hi)),
                    });
                }
                X86AbiArgLoc::Stack { offset, .. } => {
                    // 16-byte aligned outgoing slot (classifier
                    // guarantees the offset).
                    emit_load_slot_pair(mf, mb, slots, X86Reg::Rax, X86Reg::Rdx);
                    for (half, reg) in [(0i64, X86Reg::Rax), (8, X86Reg::Rdx)] {
                        push(
                            mf,
                            mb,
                            X86Opcode::MovMR,
                            OpSize::Q,
                            vec![
                                X86Operand::Reg(reg),
                                X86Operand::Mem {
                                    base: Some(X86Reg::Rsp),
                                    index: None,
                                    scale: 1,
                                    disp: offset + half,
                                },
                            ],
                            None,
                        );
                    }
                }
                other => panic!("isel: unexpected ABI loc {:?} for i128 arg", other),
            }
            continue;
        }
        let v = ctx.lookup_vreg(*arg);
        match loc {
            X86AbiArgLoc::Gp(r) => pending.push(X86Inst {
                opcode: X86Opcode::MovRR,
                size: gp_move_size(v.class),
                operands: vec![X86Operand::VReg(v)],
                def: Some(X86Operand::Reg(*r)),
            }),
            X86AbiArgLoc::Xmm(r) => pending.push(X86Inst {
                opcode: fp_move(ty),
                size: fp_size(ty),
                operands: vec![X86Operand::VReg(v)],
                def: Some(X86Operand::Reg(*r)),
            }),
            X86AbiArgLoc::Stack { offset, .. } => {
                let mem = X86Operand::Mem {
                    base: Some(X86Reg::Rsp),
                    index: None,
                    scale: 1,
                    disp: *offset,
                };
                match v.class {
                    X86RegClass::Xmm => push(
                        mf,
                        mb,
                        fp_move(ty),
                        fp_size(ty),
                        vec![X86Operand::VReg(v)],
                        Some(mem),
                    ),
                    // Slots are eightbyte-rounded; narrow ints write
                    // their Gp32 form (the significant bytes land at
                    // the slot's base, little-endian).
                    class => push(
                        mf,
                        mb,
                        X86Opcode::MovMR,
                        gp_move_size(class),
                        vec![X86Operand::VReg(v), mem],
                        None,
                    ),
                }
            }
            other => panic!("isel: unexpected ABI loc {:?} for scalar arg", other),
        }
    }
    for m in pending {
        mf.block_mut(mb).insts.push(m);
    }

    if !is_internal {
        // AL = xmm argument registers used, before every external call
        // (x04's blanket policy: exact count beats auditing which
        // externals are variadic; afs_* runtime calls are external for
        // AL purposes even though none is variadic).
        push(
            mf,
            mb,
            X86Opcode::MovRI,
            OpSize::B,
            vec![X86Operand::Imm(state.xmm_used() as i64)],
            Some(X86Operand::Reg(X86Reg::Rax)),
        );
    }
    if let Some(target) = indirect_target {
        // Stage the pointer into r11, then `call *%r11`. The MovRR
        // sits with the batched register-arg moves' tail; its source
        // vreg load (regalloc scratch r10) cannot disturb argument
        // registers.
        push(
            mf,
            mb,
            X86Opcode::MovRR,
            OpSize::Q,
            vec![X86Operand::VReg(target)],
            Some(X86Operand::Reg(X86Reg::R11)),
        );
        push(
            mf,
            mb,
            X86Opcode::CallReg,
            OpSize::Q,
            vec![X86Operand::Reg(X86Reg::R11)],
            None,
        );
    } else {
        push(
            mf,
            mb,
            X86Opcode::Call,
            OpSize::Q,
            vec![X86Operand::Extern(label)],
            None,
        );
    }

    if inst.ty != IrType::Void {
        if matches!(inst.ty, IrType::Int(IntWidth::I128)) {
            // psABI: INTEGER pair returns in rax (low) : rdx (high)
            // (abi.tex §3.2.3, returning of values).
            let dest = ctx.lookup_wide_slot(inst.id);
            emit_store_pair_to_slot(mf, mb, dest, X86Reg::Rax, X86Reg::Rdx);
            return;
        }
        if is_wide_pair_ty(&inst.ty) {
            // complex(8) return: xmm0/xmm1 into the wide slot pair.
            let dest = ctx.lookup_wide_slot(inst.id);
            for (slot, xmm) in [(dest.0, X86Reg::Xmm0), (dest.1, X86Reg::Xmm1)] {
                push(
                    mf,
                    mb,
                    X86Opcode::Movsd,
                    OpSize::Q,
                    vec![X86Operand::Reg(xmm)],
                    Some(X86Operand::FrameSlot(slot)),
                );
            }
            return;
        }
        if matches!(&inst.ty, IrType::Array(elem, 2) if matches!(elem.as_ref(), IrType::Float(FloatWidth::F32)))
        {
            // complex(4) return: packed xmm0 → Gp64 raw bits.
            let dest = ctx.lookup_vreg(inst.id);
            push(
                mf,
                mb,
                X86Opcode::MovqXmmToGp,
                OpSize::Q,
                vec![X86Operand::Reg(X86Reg::Xmm0)],
                Some(X86Operand::VReg(dest)),
            );
            return;
        }
        let dest = ctx.lookup_vreg(inst.id);
        match classify_return(&abi_ty_of(&inst.ty)) {
            X86RetLoc::Regs(regs) if regs.len() == 1 => {
                emit_phys_to_vreg(mf, mb, dest, regs[0], &inst.ty);
            }
            other => panic!("x05 scope: call return shape {:?} deferred", other),
        }
    }
}

fn emit_phys_to_vreg(mf: &mut X86Function, mb: MBlockId, dest: X86VReg, reg: X86Reg, ty: &IrType) {
    let (opcode, size) = match dest.class {
        X86RegClass::Xmm => (fp_move(ty), fp_size(ty)),
        class => (X86Opcode::MovRR, gp_move_size(class)),
    };
    push(
        mf,
        mb,
        opcode,
        size,
        vec![X86Operand::Reg(reg)],
        Some(X86Operand::VReg(dest)),
    );
}

fn select_terminator(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    term: &Terminator,
    func: &Function,
) {
    match term {
        Terminator::Return(None) => {
            push(mf, mb, X86Opcode::Ret, OpSize::Q, vec![], None);
        }
        Terminator::Return(Some(val)) => {
            let ty = func
                .value_type(*val)
                .unwrap_or_else(|| panic!("isel: missing type for return value %{}", val.0));
            if matches!(ty, IrType::Int(IntWidth::I128)) {
                // psABI (abi.tex §3.2.3, returning of values): a two-
                // eightbyte INTEGER-class value returns in rax (low)
                // and rdx (high).
                let src = ctx.lookup_wide_slot(*val);
                emit_load_slot_pair(mf, mb, src, X86Reg::Rax, X86Reg::Rdx);
                push(mf, mb, X86Opcode::Ret, OpSize::Q, vec![], None);
                return;
            }
            if is_wide_pair_ty(&ty) {
                // complex(8): two SSE eightbytes → xmm0 (real), xmm1
                // (imag) — abi.tex §3.2.3 via the classifier's
                // ComplexF64 row.
                let src = ctx.lookup_wide_slot(*val);
                for (slot, xmm) in [(src.0, X86Reg::Xmm0), (src.1, X86Reg::Xmm1)] {
                    push(
                        mf,
                        mb,
                        X86Opcode::Movsd,
                        OpSize::Q,
                        vec![X86Operand::FrameSlot(slot)],
                        Some(X86Operand::Reg(xmm)),
                    );
                }
                push(mf, mb, X86Opcode::Ret, OpSize::Q, vec![], None);
                return;
            }
            if matches!(&ty, IrType::Array(elem, 2) if matches!(elem.as_ref(), IrType::Float(FloatWidth::F32)))
            {
                // complex(4): ONE SSE eightbyte, returned PACKED in
                // xmm0's low 64 bits (NOT xmm0/xmm1 — AAPCS64 habit;
                // see AbiTy::ComplexF32 docs / abi.tex §3.2.3). The
                // value rides a Gp64 vreg as raw bits; movq it across.
                let src = ctx.lookup_vreg(*val);
                push(
                    mf,
                    mb,
                    X86Opcode::MovqGpToXmm,
                    OpSize::Q,
                    vec![X86Operand::VReg(src)],
                    Some(X86Operand::Reg(X86Reg::Xmm0)),
                );
                push(mf, mb, X86Opcode::Ret, OpSize::Q, vec![], None);
                return;
            }
            let src = ctx.lookup_vreg(*val);
            match classify_return(&abi_ty_of(&ty)) {
                X86RetLoc::Regs(regs) if regs.len() == 1 => {
                    let r = regs[0];
                    let (opcode, size) = match src.class {
                        X86RegClass::Xmm => (fp_move(&ty), fp_size(&ty)),
                        class => (X86Opcode::MovRR, gp_move_size(class)),
                    };
                    push(
                        mf,
                        mb,
                        opcode,
                        size,
                        vec![X86Operand::VReg(src)],
                        Some(X86Operand::Reg(r)),
                    );
                }
                other => panic!("x05 scope: return shape {:?} deferred", other),
            }
            push(mf, mb, X86Opcode::Ret, OpSize::Q, vec![], None);
        }
        Terminator::Branch(dest, args) => {
            emit_branch_arg_copies(mf, ctx, mb, *dest, args);
            let target = ctx.lookup_block(*dest);
            push(
                mf,
                mb,
                X86Opcode::Jmp,
                OpSize::Q,
                vec![X86Operand::BlockRef(target)],
                None,
            );
        }
        Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        } => {
            let vcond = ctx.lookup_vreg(*cond);
            let true_mb = ctx.lookup_block(*true_dest);
            let false_mb = ctx.lookup_block(*false_dest);

            push(
                mf,
                mb,
                X86Opcode::Test,
                OpSize::L,
                vec![X86Operand::VReg(vcond), X86Operand::VReg(vcond)],
                None,
            );

            // Edge copies must run only on the taken edge: an arm that
            // carries args gets a shim block (copies + jump), same
            // structure ARM isel uses.
            let true_target = if true_args.is_empty() {
                true_mb
            } else {
                let shim = mf.new_block();
                emit_branch_arg_copies(mf, ctx, shim, *true_dest, true_args);
                push(
                    mf,
                    shim,
                    X86Opcode::Jmp,
                    OpSize::Q,
                    vec![X86Operand::BlockRef(true_mb)],
                    None,
                );
                shim
            };
            push(
                mf,
                mb,
                X86Opcode::Jcc,
                OpSize::Q,
                vec![
                    X86Operand::Cond(X86Cond::Ne),
                    X86Operand::BlockRef(true_target),
                ],
                None,
            );

            let false_target = if false_args.is_empty() {
                false_mb
            } else {
                let shim = mf.new_block();
                emit_branch_arg_copies(mf, ctx, shim, *false_dest, false_args);
                push(
                    mf,
                    shim,
                    X86Opcode::Jmp,
                    OpSize::Q,
                    vec![X86Operand::BlockRef(false_mb)],
                    None,
                );
                shim
            };
            push(
                mf,
                mb,
                X86Opcode::Jmp,
                OpSize::Q,
                vec![X86Operand::BlockRef(false_target)],
                None,
            );
        }
        Terminator::Switch {
            selector,
            cases,
            default,
        } => {
            let sel = ctx.lookup_vreg(*selector);
            let size = match func.value_type(*selector) {
                Some(IrType::Int(IntWidth::I64)) => OpSize::Q,
                _ => OpSize::L,
            };
            let default_mb = ctx.lookup_block(*default);
            for (val, dest) in cases {
                let dest_mb = ctx.lookup_block(*dest);
                push(
                    mf,
                    mb,
                    X86Opcode::Cmp,
                    size,
                    vec![X86Operand::VReg(sel), X86Operand::Imm(*val)],
                    None,
                );
                push(
                    mf,
                    mb,
                    X86Opcode::Jcc,
                    OpSize::Q,
                    vec![X86Operand::Cond(X86Cond::E), X86Operand::BlockRef(dest_mb)],
                    None,
                );
            }
            push(
                mf,
                mb,
                X86Opcode::Jmp,
                OpSize::Q,
                vec![X86Operand::BlockRef(default_mb)],
                None,
            );
        }
        Terminator::Unreachable => {
            // Follows a STOP/ERROR STOP runtime call that never
            // returns; `ret` keeps the assembler happy without
            // inventing a trap opcode (ARM uses brk here).
            push(mf, mb, X86Opcode::Ret, OpSize::Q, vec![], None);
        }
    }
}

/// Materialize branch arguments into the target block's param vregs.
/// IR semantics are a parallel copy; staging every source through a
/// fresh temp vreg first means all reads complete before any param is
/// written, so overlapping/swapped args can't clobber each other.
/// Simple and safe at -O0 — the coalescer-friendly safe-order walk
/// ARM uses is an optimization, not a correctness requirement.
fn emit_branch_arg_copies(
    mf: &mut X86Function,
    ctx: &X86ISelCtx,
    mb: MBlockId,
    target_block: BlockId,
    args: &[ValueId],
) {
    // (i128 branch args are slot-to-slot pair copies, handled in the
    // per-arg loop below by the wide_slots lookup.)
    if args.is_empty() {
        return;
    }
    let target_params = ctx
        .block_params
        .get(&target_block)
        .expect("isel: branch target not in block_params snapshot");
    assert_eq!(
        target_params.len(),
        args.len(),
        "isel: branch arg count ≠ target block param count"
    );

    let mut staged: Vec<(X86VReg, X86VReg)> = Vec::with_capacity(args.len());
    for (arg, bp) in args.iter().zip(target_params.iter()) {
        if is_wide_pair_ty(&bp.ty) {
            // Wide branch arg: slot-pair copy through rax:rdx. Slots
            // are per-value, so source and destination never alias
            // unless they are literally the same value (skip then).
            let dst = ctx.lookup_wide_slot(bp.id);
            let src = ctx.lookup_wide_slot(*arg);
            if dst == src {
                continue;
            }
            emit_load_slot_pair(mf, mb, src, X86Reg::Rax, X86Reg::Rdx);
            emit_store_pair_to_slot(mf, mb, dst, X86Reg::Rax, X86Reg::Rdx);
            continue;
        }
        let dst = ctx.lookup_vreg(bp.id);
        let src = ctx.lookup_vreg(*arg);
        if dst == src {
            continue;
        }
        let tmp = mf.new_vreg(dst.class);
        emit_vreg_move(mf, mb, tmp, src);
        staged.push((dst, tmp));
    }
    for (dst, tmp) in staged {
        emit_vreg_move(mf, mb, dst, tmp);
    }
}

// ---- Type helpers ----

fn type_to_class(ty: &IrType) -> X86RegClass {
    match ty {
        IrType::Float(_) => X86RegClass::Xmm,
        // 8-byte by-value arrays (complex(4) copies: [f32 x 2]) are
        // raw quad bits in a GP register; 16-byte ones take the
        // wide-slot path before classing is consulted.
        IrType::Array(elem, n) if scalar_elem_bytes(elem) * n == 8 => X86RegClass::Gp64,
        IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_) => X86RegClass::Gp64,
        IrType::Int(_) | IrType::Bool => X86RegClass::Gp32,
        // 128-bit SSE vectors (x10).
        IrType::Vector { .. } => X86RegClass::Xmm128,
        other => panic!("x05 scope: no register class for {:?}", other),
    }
}

fn reject_unsupported(ty: &IrType) {
    // type_to_class panics with the scoped message; calling it for the
    // side effect keeps the rejection list in one place.
    let _ = type_to_class(ty);
}

/// ALU operand size for a value's type (narrow ints run at L).
fn op_size(ty: &IrType) -> OpSize {
    match ty {
        IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_) => OpSize::Q,
        IrType::Float(FloatWidth::F64) => OpSize::Q,
        _ => OpSize::L,
    }
}

fn gp_move_size(class: X86RegClass) -> OpSize {
    match class {
        X86RegClass::Gp64 => OpSize::Q,
        X86RegClass::Gp8 => OpSize::B,
        _ => OpSize::L,
    }
}

/// Element type of a vector IR type (x10 packed lowering).
fn vec_elem_of(ty: &IrType) -> &IrType {
    match ty {
        IrType::Vector { elem, .. } => elem,
        other => panic!("x10: expected vector type, got {:?}", other),
    }
}

/// Load a per-lane float sign/abs mask from .rodata into a fresh
/// vector vreg. `sign` picks 0x8000.. (negate) vs 0x7fff.. (abs).
fn vec_mask_vreg(
    mf: &mut X86Function,
    _ctx: &X86ISelCtx,
    mb: MBlockId,
    width: &FloatWidth,
    sign: bool,
) -> X86VReg {
    let mut bytes = [0u8; 16];
    match width {
        FloatWidth::F32 => {
            let lane: u32 = if sign { 0x8000_0000 } else { 0x7fff_ffff };
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&lane.to_le_bytes());
            }
        }
        FloatWidth::F64 => {
            let lane: u64 = if sign {
                0x8000_0000_0000_0000
            } else {
                0x7fff_ffff_ffff_ffff
            };
            for i in 0..2 {
                bytes[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
            }
        }
    }
    let label = mf.add_rodata_bytes(&bytes);
    let mask = mf.new_vreg(X86RegClass::Xmm128);
    push(
        mf,
        mb,
        X86Opcode::Movups,
        OpSize::Q,
        vec![X86Operand::RipLabel(label)],
        Some(X86Operand::VReg(mask)),
    );
    mask
}

/// Packed i32 compare via pcmpgtd/pcmpeqd: Eq/Gt native, Lt a swap,
/// Ne/Ge/Le by inverting with an all-ones mask.
fn emit_packed_icmp(
    mf: &mut X86Function,
    _ctx: &X86ISelCtx,
    mb: MBlockId,
    op: CmpOp,
    va: X86VReg,
    vb: X86VReg,
    dest: X86VReg,
) {
    let direct = |mf: &mut X86Function, opc, l: X86VReg, r: X86VReg, d: X86VReg| {
        mf.block_mut(mb).insts.push(X86Inst {
            opcode: opc,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(l), X86Operand::VReg(r)],
            def: Some(X86Operand::VReg(d)),
        });
    };
    let inverted = |mf: &mut X86Function, opc, l: X86VReg, r: X86VReg, d: X86VReg| {
        let raw = mf.new_vreg(X86RegClass::Xmm128);
        direct(mf, opc, l, r, raw);
        let ones_label = mf.add_rodata_bytes(&[0xffu8; 16]);
        let ones = mf.new_vreg(X86RegClass::Xmm128);
        mf.block_mut(mb).insts.push(X86Inst {
            opcode: X86Opcode::Movups,
            size: OpSize::Q,
            operands: vec![X86Operand::RipLabel(ones_label)],
            def: Some(X86Operand::VReg(ones)),
        });
        direct(mf, X86Opcode::Pxor, raw, ones, d);
    };
    match op {
        CmpOp::Eq => direct(mf, X86Opcode::Pcmpeqd, va, vb, dest),
        CmpOp::Gt => direct(mf, X86Opcode::Pcmpgtd, va, vb, dest),
        CmpOp::Lt => direct(mf, X86Opcode::Pcmpgtd, vb, va, dest),
        CmpOp::Ne => inverted(mf, X86Opcode::Pcmpeqd, va, vb, dest),
        CmpOp::Ge => inverted(mf, X86Opcode::Pcmpgtd, vb, va, dest),
        CmpOp::Le => inverted(mf, X86Opcode::Pcmpgtd, va, vb, dest),
    }
}

/// Packed signed i32 min/max synthesis for SSE2 (no pminsd/pmaxsd until
/// SSE4.1). Compare-and-blend: a signed `pcmpgtd` mask selects between
/// the two operands with `pand`/`pandn`/`por`. This is the sequence
/// gfortran and LLVM/flang both emit at the SSE2 baseline (LLVM marks
/// SMIN/SMAX v4i32 Legal at SSE2 and expands to exactly this).
///
///   max(a,b):  mask = a>b;  (mask & a) | (~mask & b)
///   min(a,b):  mask = a>b;  (mask & b) | (~mask & a)
///
/// `pandn op0,op1` computes `(~op0) & op1`. All ops are emitted
/// three-address; two-address conversion inserts the tie copies, so the
/// shared `mask` is preserved across both blends.
fn emit_packed_iminmax(
    mf: &mut X86Function,
    mb: MBlockId,
    is_min: bool,
    va: X86VReg,
    vb: X86VReg,
    dest: X86VReg,
) {
    let push = |mf: &mut X86Function, opc, l: X86VReg, r: X86VReg, d: X86VReg| {
        mf.block_mut(mb).insts.push(X86Inst {
            opcode: opc,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(l), X86Operand::VReg(r)],
            def: Some(X86Operand::VReg(d)),
        });
    };
    let mask = mf.new_vreg(X86RegClass::Xmm128);
    push(mf, X86Opcode::Pcmpgtd, va, vb, mask); // mask = a > b
    let keep = mf.new_vreg(X86RegClass::Xmm128); // mask & (max?a:b)
    let drop = mf.new_vreg(X86RegClass::Xmm128); // ~mask & (max?b:a)
    if is_min {
        push(mf, X86Opcode::Pand, mask, vb, keep); // a>b -> b
        push(mf, X86Opcode::Pandn, mask, va, drop); // a<=b -> a
    } else {
        push(mf, X86Opcode::Pand, mask, va, keep); // a>b -> a
        push(mf, X86Opcode::Pandn, mask, vb, drop); // a<=b -> b
    }
    push(mf, X86Opcode::Por, keep, drop, dest);
}

/// Packed signed i32 abs synthesis for SSE2 (no `pabsd` until SSSE3).
/// The sign mask + xor-sub idiom gfortran and LLVM emit at the SSE2
/// baseline: `mask = (0 > x)` (all-ones where x is negative), then
/// `(x ^ mask) - mask`. For x<0 mask=-1 so it yields `~x - (-1) = -x`;
/// for x>=0 mask=0 so it yields `x`. `pcmpgtd` is a signed compare, so
/// the mask is the arithmetic sign mask (the `psrad $31` LLVM uses, which
/// we lack as a packed opcode). Zero comes from rodata, not a self-xor,
/// to avoid a read-before-def of an undefined vreg.
fn emit_packed_iabs(mf: &mut X86Function, mb: MBlockId, vx: X86VReg, dest: X86VReg) {
    let push = |mf: &mut X86Function, opc, l: X86VReg, r: X86VReg, d: X86VReg| {
        mf.block_mut(mb).insts.push(X86Inst {
            opcode: opc,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(l), X86Operand::VReg(r)],
            def: Some(X86Operand::VReg(d)),
        });
    };
    let zero_label = mf.add_rodata_bytes(&[0u8; 16]);
    let zero = mf.new_vreg(X86RegClass::Xmm128);
    mf.block_mut(mb).insts.push(X86Inst {
        opcode: X86Opcode::Movups,
        size: OpSize::Q,
        operands: vec![X86Operand::RipLabel(zero_label)],
        def: Some(X86Operand::VReg(zero)),
    });
    let mask = mf.new_vreg(X86RegClass::Xmm128);
    push(mf, X86Opcode::Pcmpgtd, zero, vx, mask); // mask = 0 > x  (x < 0)
    let xored = mf.new_vreg(X86RegClass::Xmm128);
    push(mf, X86Opcode::Pxor, vx, mask, xored); // x ^ mask
    push(mf, X86Opcode::Psubd, xored, mask, dest); // (x ^ mask) - mask
}

/// One step of a packed across-lane reduction tree: either a single
/// packed op (sum, float min/max) or the synthesized signed i32 min/max.
#[derive(Clone, Copy)]
enum ReduceStep {
    Op(X86Opcode),
    /// `true` = min, `false` = max.
    IntMinMax(bool),
}

fn emit_reduce_step(
    mf: &mut X86Function,
    mb: MBlockId,
    step: ReduceStep,
    a: X86VReg,
    b: X86VReg,
    dest: X86VReg,
) {
    match step {
        ReduceStep::Op(opc) => {
            mf.block_mut(mb).insts.push(X86Inst {
                opcode: opc,
                size: OpSize::Q,
                operands: vec![X86Operand::VReg(a), X86Operand::VReg(b)],
                def: Some(X86Operand::VReg(dest)),
            });
        }
        ReduceStep::IntMinMax(is_min) => emit_packed_iminmax(mf, mb, is_min, a, b, dest),
    }
}

fn float_width(ty: &IrType) -> FloatWidth {
    match ty {
        IrType::Float(w) => *w,
        other => panic!("isel: expected a float type, got {:?}", other),
    }
}

fn fp_move_w(width: FloatWidth) -> X86Opcode {
    match width {
        FloatWidth::F32 => X86Opcode::Movss,
        FloatWidth::F64 => X86Opcode::Movsd,
    }
}

fn fp_size_w(width: FloatWidth) -> OpSize {
    match width {
        FloatWidth::F32 => OpSize::L,
        FloatWidth::F64 => OpSize::Q,
    }
}

fn fp_move(ty: &IrType) -> X86Opcode {
    fp_move_w(float_width(ty))
}

fn fp_size(ty: &IrType) -> OpSize {
    fp_size_w(float_width(ty))
}

/// i128 compare via the sub/sbb flags idiom — no branches:
///   Eq/Ne:  xor-diff both halves, OR them, set{e,ne} on ZF.
///   Lt/Ge:  rax:rdx = lhs; subq rhs.lo; sbbq rhs.hi; SF!=OF iff
///           lhs < rhs (signed)  → setl / setge.
///   Gt/Le:  the same idiom with the operands swapped (rhs < lhs).
/// Everything stays in phys regs + frame slots, so the allocator
/// cannot break the sub/sbb flag adjacency.
fn emit_i128_icmp(
    mf: &mut X86Function,
    ctx: &mut X86ISelCtx,
    mb: MBlockId,
    op: CmpOp,
    a: ValueId,
    b: ValueId,
    dest: X86VReg,
) {
    use X86Reg::{Rax, Rdx};
    let (first, second, cond) = match op {
        CmpOp::Eq => (a, b, X86Cond::E),
        CmpOp::Ne => (a, b, X86Cond::Ne),
        CmpOp::Lt => (a, b, X86Cond::L),
        CmpOp::Ge => (a, b, X86Cond::Ge),
        // lhs > rhs  ==  rhs < lhs;  lhs <= rhs  ==  !(rhs < lhs)
        CmpOp::Gt => (b, a, X86Cond::L),
        CmpOp::Le => (b, a, X86Cond::Ge),
    };
    let fs = ctx.lookup_wide_slot(first);
    let ss = ctx.lookup_wide_slot(second);
    emit_load_slot_pair(mf, mb, fs, Rax, Rdx);
    match op {
        CmpOp::Eq | CmpOp::Ne => {
            emit_alu_reg_slot(mf, mb, X86Opcode::Xor, Rax, ss.0);
            emit_alu_reg_slot(mf, mb, X86Opcode::Xor, Rdx, ss.1);
            push(
                mf,
                mb,
                X86Opcode::Or,
                OpSize::Q,
                vec![X86Operand::Reg(Rax), X86Operand::Reg(Rdx)],
                Some(X86Operand::Reg(Rax)),
            );
        }
        _ => {
            emit_alu_reg_slot(mf, mb, X86Opcode::Sub, Rax, ss.0);
            emit_alu_reg_slot(mf, mb, X86Opcode::Sbb, Rdx, ss.1);
        }
    }
    emit_setcc_movzx(mf, mb, cond, dest);
}

/// Byte size of a scalar element type, layout-independent (floats and
/// fixed ints only — exactly the element types by-value array copies
/// carry).
fn scalar_elem_bytes(ty: &IrType) -> u64 {
    match ty {
        IrType::Bool | IrType::Int(IntWidth::I8) => 1,
        IrType::Int(IntWidth::I16) => 2,
        IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32) => 4,
        IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => 8,
        IrType::Int(IntWidth::I128) => 16,
        _ => 0,
    }
}

/// 16-byte values that ride the wide-slot pair machinery: i128 and
/// by-value 16-byte arrays (complex(8) copies appear in IR as
/// `load/store : [f64 x 2]`). Raw bit pairs either way.
fn is_wide_pair_ty(ty: &IrType) -> bool {
    match ty {
        IrType::Int(IntWidth::I128) => true,
        IrType::Array(elem, n) => scalar_elem_bytes(elem) * n == 16,
        _ => false,
    }
}

// ---- i128 slot helpers (x08) ----
//
// i128 values are memory-resident (lo, hi) frame-slot pairs (the
// arm64 wide-slot model adapted to x05's operand grammar). Staging
// goes through rax:rdx (rcx/rsi for second pairs and pointer bases) —
// physical registers the naive allocator never touches. Emitted
// instructions mix only phys regs and frame slots, so no spill
// traffic can land between a flags producer (Add/Sub on the low
// quad) and its Adc/Sbb consumer.

fn emit_load_slot_pair(
    mf: &mut X86Function,
    mb: MBlockId,
    slots: (i32, i32),
    lo: X86Reg,
    hi: X86Reg,
) {
    for (slot, reg) in [(slots.0, lo), (slots.1, hi)] {
        push(
            mf,
            mb,
            X86Opcode::MovRM,
            OpSize::Q,
            vec![X86Operand::FrameSlot(slot)],
            Some(X86Operand::Reg(reg)),
        );
    }
}

fn emit_store_pair_to_slot(
    mf: &mut X86Function,
    mb: MBlockId,
    slots: (i32, i32),
    lo: X86Reg,
    hi: X86Reg,
) {
    for (slot, reg) in [(slots.0, lo), (slots.1, hi)] {
        push(
            mf,
            mb,
            X86Opcode::MovMR,
            OpSize::Q,
            vec![X86Operand::Reg(reg), X86Operand::FrameSlot(slot)],
            None,
        );
    }
}

/// movabsq both halves of a constant into (lo, hi).
fn emit_const_i128_pair(mf: &mut X86Function, mb: MBlockId, val: i128, lo: X86Reg, hi: X86Reg) {
    let bits = val as u128;
    for (half, reg) in [(bits as u64, lo), ((bits >> 64) as u64, hi)] {
        push(
            mf,
            mb,
            X86Opcode::MovRI,
            OpSize::Q,
            vec![X86Operand::Imm(half as i64)],
            Some(X86Operand::Reg(reg)),
        );
    }
}

/// Tied ALU op `reg <op>= slot` (the operand-1 memory form).
fn emit_alu_reg_slot(
    mf: &mut X86Function,
    mb: MBlockId,
    opcode: X86Opcode,
    reg: X86Reg,
    slot: i32,
) {
    push(
        mf,
        mb,
        opcode,
        OpSize::Q,
        vec![X86Operand::Reg(reg), X86Operand::FrameSlot(slot)],
        Some(X86Operand::Reg(reg)),
    );
}

/// IrType → ABI scalar for the x04 classifier. Scalars only — x05's
/// call scope.
fn abi_ty_of(ty: &IrType) -> AbiTy {
    match ty {
        IrType::Bool => AbiTy::Scalar(AbiField::Bool),
        IrType::Int(IntWidth::I8) => AbiTy::Scalar(AbiField::I8),
        IrType::Int(IntWidth::I16) => AbiTy::Scalar(AbiField::I16),
        IrType::Int(IntWidth::I32) => AbiTy::Scalar(AbiField::I32),
        IrType::Int(IntWidth::I64) => AbiTy::Scalar(AbiField::I64),
        IrType::Int(IntWidth::I128) => AbiTy::Scalar(AbiField::I128),
        IrType::Float(FloatWidth::F32) => AbiTy::Scalar(AbiField::F32),
        IrType::Float(FloatWidth::F64) => AbiTy::Scalar(AbiField::F64),
        IrType::Ptr(_) | IrType::FuncPtr(_) => AbiTy::Scalar(AbiField::Ptr),
        // Fortran COMPLEX reaches the boundary as a by-value float
        // pair; the classifier owns the eightbyte split (complex(4)
        // is ONE SSE eightbyte, abi.tex §3.2.3 — see AbiTy docs).
        IrType::Array(elem, 2) if matches!(elem.as_ref(), IrType::Float(FloatWidth::F32)) => {
            AbiTy::ComplexF32
        }
        IrType::Array(elem, 2) if matches!(elem.as_ref(), IrType::Float(FloatWidth::F64)) => {
            AbiTy::ComplexF64
        }
        other => panic!("x05 scope: non-scalar ABI type {:?} deferred", other),
    }
}

/// Stack-slot size for an alloca. Mirrors ARM isel's `alloca_size`
/// (Bool slots stay 4 bytes wide; loads/stores still touch 1).
fn alloca_size(ty: &IrType, layout: crate::target::TargetLayout) -> u64 {
    match ty {
        IrType::Void => 0,
        IrType::Bool => 4,
        IrType::Int(w) => w.bytes() as u64,
        IrType::Float(w) => w.bytes() as u64,
        IrType::Ptr(_) | IrType::FuncPtr(_) => 8,
        IrType::Array(elem, count) => {
            let elem_size = match elem.as_ref() {
                IrType::Bool => 4,
                IrType::Struct(_) => alloca_size(elem, layout),
                _ => elem.size_bytes(&layout),
            };
            elem_size * count
        }
        IrType::Struct(_) => 8, // placeholder, mirrors ARM
        IrType::Vector { .. } => 16,
    }
}

/// Alignment ladder 4 → 8 → 16 by size, same as ARM's frame slots.
fn slot_align(size: u64) -> u64 {
    if size >= 16 {
        16
    } else if size >= 8 {
        8
    } else {
        4
    }
}

/// C-level symbol for a runtime function. Same table as ARM isel,
/// minus the i128 print variant (x05 scope gates i128 earlier).
fn runtime_func_symbol(rf: &RuntimeFunc, args: &[ValueId], func: &Function) -> String {
    match rf {
        RuntimeFunc::PrintInt => {
            if args
                .first()
                .and_then(|a| func.value_type(*a))
                .is_some_and(|ty| matches!(ty, IrType::Int(IntWidth::I64)))
            {
                "afs_print_int64".into()
            } else {
                "afs_print_int".into()
            }
        }
        RuntimeFunc::PrintReal => "afs_print_real".into(),
        RuntimeFunc::PrintString => "afs_print_string".into(),
        RuntimeFunc::PrintLogical => "afs_print_logical".into(),
        RuntimeFunc::PrintNewline => "afs_print_newline".into(),
        RuntimeFunc::Allocate => "afs_allocate".into(),
        RuntimeFunc::Deallocate => "afs_deallocate".into(),
        RuntimeFunc::StringConcat => "afs_string_concat".into(),
        RuntimeFunc::StringCopy => "afs_string_copy".into(),
        RuntimeFunc::StringCompare => "afs_string_compare".into(),
        RuntimeFunc::Stop => "afs_stop".into(),
        RuntimeFunc::ErrorStop => "afs_error_stop".into(),
        RuntimeFunc::CheckBounds => "afs_check_bounds".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::builder::FuncBuilder;

    fn select_simple(build: impl FnOnce(&mut FuncBuilder)) -> X86Function {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            build(&mut b);
        }
        select_function(
            &func,
            &["test".to_string()],
            crate::target::TargetLayout::LP64,
        )
    }

    fn all_insts(mf: &X86Function) -> Vec<&X86Inst> {
        mf.blocks.iter().flat_map(|b| b.insts.iter()).collect()
    }

    // ---- The FP condition table, row by row (sprint deliverable 4).
    // `>`/`>=` are A/AE direct; `<`/`<=` swap then A/AE (never B/BE,
    // which are true on NaN); `==` is E∧NP; `/=` is NE∨P.

    #[test]
    fn fp_cond_gt_is_a_unswapped() {
        assert_eq!(
            fp_cond_plan(CmpOp::Gt),
            FpCmpPlan::Direct {
                swap_operands: false,
                cond: X86Cond::A
            }
        );
    }

    #[test]
    fn fp_cond_ge_is_ae_unswapped() {
        assert_eq!(
            fp_cond_plan(CmpOp::Ge),
            FpCmpPlan::Direct {
                swap_operands: false,
                cond: X86Cond::Ae
            }
        );
    }

    #[test]
    fn fp_cond_lt_swaps_then_a() {
        assert_eq!(
            fp_cond_plan(CmpOp::Lt),
            FpCmpPlan::Direct {
                swap_operands: true,
                cond: X86Cond::A
            }
        );
    }

    #[test]
    fn fp_cond_le_swaps_then_ae() {
        assert_eq!(
            fp_cond_plan(CmpOp::Le),
            FpCmpPlan::Direct {
                swap_operands: true,
                cond: X86Cond::Ae
            }
        );
    }

    #[test]
    fn fp_cond_eq_is_e_and_np() {
        assert_eq!(
            fp_cond_plan(CmpOp::Eq),
            FpCmpPlan::Equality {
                cond: X86Cond::E,
                parity: X86Cond::Np,
                combine: X86Opcode::And
            }
        );
    }

    #[test]
    fn fp_cond_ne_is_ne_or_p() {
        assert_eq!(
            fp_cond_plan(CmpOp::Ne),
            FpCmpPlan::Equality {
                cond: X86Cond::Ne,
                parity: X86Cond::P,
                combine: X86Opcode::Or
            }
        );
    }

    #[test]
    fn fcmp_eq_emits_sete_setnp_and_movzx() {
        let mf = select_simple(|b| {
            let x = b.const_f64(1.0);
            let y = b.const_f64(2.0);
            b.fcmp(CmpOp::Eq, x, y);
            b.ret_void();
        });
        let insts = all_insts(&mf);
        let conds: Vec<X86Cond> = insts
            .iter()
            .filter(|i| i.opcode == X86Opcode::Setcc)
            .map(|i| match i.operands[0] {
                X86Operand::Cond(c) => c,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(conds, vec![X86Cond::E, X86Cond::Np]);
        assert!(insts.iter().any(|i| i.opcode == X86Opcode::And));
        assert!(insts
            .iter()
            .any(|i| matches!(i.opcode, X86Opcode::Movzx { src: OpSize::B })));
    }

    #[test]
    fn fcmp_lt_swaps_ucomi_operands() {
        let mf = select_simple(|b| {
            let x = b.const_f64(1.0);
            let y = b.const_f64(2.0);
            b.fcmp(CmpOp::Lt, x, y);
            b.ret_void();
        });
        let insts = all_insts(&mf);
        let ucomi = insts
            .iter()
            .find(|i| i.opcode == X86Opcode::Ucomisd)
            .expect("ucomisd emitted");
        // x → v0, y → v1 (phase 4a order); Lt compares swapped: y vs x.
        let ids: Vec<u32> = ucomi
            .operands
            .iter()
            .map(|o| match o {
                X86Operand::VReg(v) => v.id.0,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(ids, vec![1, 0]);
        // Direct plan: exactly one setcc, with the A condition.
        let setccs: Vec<&&X86Inst> = insts
            .iter()
            .filter(|i| i.opcode == X86Opcode::Setcc)
            .collect();
        assert_eq!(setccs.len(), 1);
        assert_eq!(setccs[0].operands[0], X86Operand::Cond(X86Cond::A));
    }

    #[test]
    fn icmp_pairs_setcc_with_movzx() {
        let mf = select_simple(|b| {
            let x = b.const_i32(1);
            let y = b.const_i32(2);
            b.icmp(CmpOp::Lt, x, y);
            b.ret_void();
        });
        let insts = all_insts(&mf);
        let pos = insts
            .iter()
            .position(|i| i.opcode == X86Opcode::Setcc)
            .expect("setcc emitted");
        assert_eq!(insts[pos].operands[0], X86Operand::Cond(X86Cond::L));
        // Stale-high-bits pitfall: the very next instruction widens.
        assert!(matches!(
            insts[pos + 1].opcode,
            X86Opcode::Movzx { src: OpSize::B }
        ));
    }

    #[test]
    fn idiv_runs_through_rax_with_vreg_divisor() {
        let mf = select_simple(|b| {
            let x = b.const_i32(7);
            let y = b.const_i32(2);
            b.idiv(x, y);
            b.ret_void();
        });
        let insts = all_insts(&mf);
        assert!(insts
            .iter()
            .any(|i| i.opcode == X86Opcode::MovRR && i.def == Some(X86Operand::Reg(X86Reg::Rax))));
        assert!(insts.iter().any(|i| i.opcode == X86Opcode::Cltd));
        let idiv = insts
            .iter()
            .find(|i| i.opcode == X86Opcode::Idiv)
            .expect("idiv emitted");
        // Divisor copied to a fresh vreg — never a phys reg operand.
        assert!(matches!(idiv.operands[0], X86Operand::VReg(_)));
        // Quotient comes out of rax.
        assert!(insts.iter().any(|i| i.opcode == X86Opcode::MovRR
            && i.operands[0] == X86Operand::Reg(X86Reg::Rax)
            && matches!(i.def, Some(X86Operand::VReg(_)))));
    }

    #[test]
    fn imod_reads_remainder_from_rdx() {
        let mf = select_simple(|b| {
            let x = b.const_i64(7);
            let y = b.const_i64(2);
            b.imod(x, y);
            b.ret_void();
        });
        let insts = all_insts(&mf);
        assert!(insts.iter().any(|i| i.opcode == X86Opcode::Cqto));
        assert!(insts.iter().any(|i| i.opcode == X86Opcode::MovRR
            && i.operands[0] == X86Operand::Reg(X86Reg::Rdx)
            && matches!(i.def, Some(X86Operand::VReg(_)))));
    }

    #[test]
    fn shift_count_moves_through_rcx() {
        let mf = select_simple(|b| {
            let x = b.const_i32(1);
            let n = b.const_i32(3);
            b.shl(x, n);
            b.ret_void();
        });
        let insts = all_insts(&mf);
        assert!(insts
            .iter()
            .any(|i| i.opcode == X86Opcode::MovRR && i.def == Some(X86Operand::Reg(X86Reg::Rcx))));
        let shl = insts
            .iter()
            .find(|i| i.opcode == X86Opcode::Shl)
            .expect("shl emitted");
        assert_eq!(shl.operands[1], X86Operand::Reg(X86Reg::Rcx));
    }

    #[test]
    fn const_float_lands_in_rodata() {
        let mf = select_simple(|b| {
            b.const_f64(3.25);
            b.ret_void();
        });
        assert_eq!(mf.rodata.len(), 1);
        assert_eq!(mf.rodata[0].1, 3.25f64.to_bits());
        let insts = all_insts(&mf);
        assert!(insts
            .iter()
            .any(|i| i.opcode == X86Opcode::Movsd
                && matches!(i.operands[0], X86Operand::RipLabel(_))));
    }

    #[test]
    fn fneg_xors_against_sign_mask() {
        let mf = select_simple(|b| {
            let x = b.const_f64(1.5);
            b.fneg(x);
            b.ret_void();
        });
        assert!(mf
            .rodata
            .iter()
            .any(|(_, bits)| *bits == 0x8000_0000_0000_0000));
        assert!(all_insts(&mf).iter().any(|i| i.opcode == X86Opcode::Xorpd));
    }

    #[test]
    fn fabs_ands_against_abs_mask() {
        let mf = select_simple(|b| {
            let x = b.const_f32(-1.5);
            b.fabs(x);
            b.ret_void();
        });
        assert!(mf.rodata.iter().any(|(_, bits)| *bits == 0x7fff_ffff));
        assert!(all_insts(&mf).iter().any(|i| i.opcode == X86Opcode::Andps));
    }

    #[test]
    fn external_call_sets_al_internal_skips_it() {
        let al_mov = |mf: &X86Function| {
            all_insts(mf)
                .iter()
                .any(|i| {
                    i.opcode == X86Opcode::MovRI
                        && i.size == OpSize::B
                        && i.def == Some(X86Operand::Reg(X86Reg::Rax))
                })
                .to_owned()
        };
        let ext = select_simple(|b| {
            let x = b.const_f64(1.0);
            b.call(
                FuncRef::External("sin".into()),
                vec![x],
                IrType::Float(FloatWidth::F64),
            );
            b.ret_void();
        });
        let al = all_insts(&ext)
            .into_iter()
            .find(|i| {
                i.opcode == X86Opcode::MovRI
                    && i.size == OpSize::B
                    && i.def == Some(X86Operand::Reg(X86Reg::Rax))
            })
            .expect("external call must set AL");
        assert_eq!(al.operands[0], X86Operand::Imm(1)); // one xmm arg

        let int = select_simple(|b| {
            b.call(FuncRef::Internal(0), vec![], IrType::Void);
            b.ret_void();
        });
        assert!(!al_mov(&int), "internal call must not set AL");
    }

    #[test]
    fn branch_args_copy_through_fresh_temps() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        let (param_id, arg_id);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let next = b.create_block("next");
            param_id = b.add_block_param(next, IrType::Int(IntWidth::I32));
            arg_id = b.const_i32(7);
            b.branch(next, vec![arg_id]);
            b.set_block(next);
            b.ret_void();
        }
        let mf = select_function(
            &func,
            &["test".to_string()],
            crate::target::TargetLayout::LP64,
        );
        let _ = (param_id, arg_id);
        // One arg → two moves (src→temp, temp→param) before the jmp.
        let entry = &mf.blocks[0].insts;
        let movs: Vec<&X86Inst> = entry
            .iter()
            .filter(|i| i.opcode == X86Opcode::MovRR)
            .collect();
        assert_eq!(movs.len(), 2);
        assert_eq!(movs[1].operands[0], movs[0].def.clone().unwrap());
        assert_eq!(entry.last().unwrap().opcode, X86Opcode::Jmp);
    }

    #[test]
    fn select_lowers_to_diamond_with_join_cursor() {
        let mf = select_simple(|b| {
            let c = b.const_bool(true);
            let x = b.const_i32(1);
            let y = b.const_i32(2);
            let s = b.select(c, x, y);
            let _ = b.iadd(s, s); // must land in the join block
            b.ret_void();
        });
        // entry + false arm + join.
        assert_eq!(mf.blocks.len(), 3);
        let entry = &mf.blocks[0].insts;
        assert!(entry.iter().any(|i| i.opcode == X86Opcode::Test));
        assert!(entry.iter().any(|i| i.opcode == X86Opcode::Jcc));
        let join = mf.blocks.last().unwrap();
        assert!(join.insts.iter().any(|i| i.opcode == X86Opcode::Add));
        assert!(join.insts.iter().any(|i| i.opcode == X86Opcode::Ret));
    }
}
