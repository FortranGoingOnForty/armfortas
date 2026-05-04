//! Instruction selection — translate SSA IR to Machine IR.
//!
//! Maps each IR instruction to one or more ARM64 machine instructions.
//! Uses virtual registers throughout; physical register assignment
//! happens in the register allocator (Sprint 21).
//!
//! Strategy: naive spill-everything. Every vreg lives on the stack.
//! Load before use, store after def. Correct but slow — optimized later.

use super::mir::*;
use crate::ir::inst::*;
use crate::ir::types::*;
use std::collections::{HashMap, HashSet};

/// Select machine instructions for an entire IR module.
pub fn select_module(module: &Module) -> Vec<MachineFunction> {
    // Build function name table for resolving Internal call refs.
    let func_names: Vec<String> = module.functions.iter().map(|f| f.name.clone()).collect();
    module
        .functions
        .iter()
        .map(|f| select_function_with_names(f, &func_names))
        .collect()
}

fn select_function_with_names(func: &Function, func_names: &[String]) -> MachineFunction {
    let mut mf = select_function(func);
    // Resolve any Internal call references to actual function names.
    for block in &mut mf.blocks {
        for inst in &mut block.insts {
            if let super::mir::ArmOpcode::Bl = inst.opcode {
                if let Some(super::mir::MachineOperand::Extern(ref mut name)) =
                    inst.operands.first_mut()
                {
                    // Check if this is a placeholder "_func_N" name from isel.
                    if name.starts_with("_func_") {
                        if let Ok(idx) = name[6..].parse::<usize>() {
                            if idx < func_names.len() {
                                *name = func_names[idx].clone();
                            }
                        }
                    }
                }
            }
        }
    }
    mf
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbiArgLoc {
    Gp(u8),
    Gp32(u8),
    Fp(u8),
    Fp32(u8),
    GpPair(u8),
    Stack(i64),
}

#[derive(Debug, Default, Clone, Copy)]
struct AbiArgState {
    gp_idx: u8,
    fp_idx: u8,
    stack_offset: i64,
}

fn align_to(value: i64, align: i64) -> i64 {
    debug_assert!(align > 0 && (align & (align - 1)) == 0);
    (value + align - 1) & !(align - 1)
}

fn abi_stack_layout(ty: &IrType) -> (i64, i64) {
    match ty {
        IrType::Int(IntWidth::I128) => (16, 16),
        IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_) => (8, 8),
        IrType::Float(FloatWidth::F64) => (8, 8),
        // TODO: integer(c_short), value actuals still appear widened before
        // reaching call lowering, so end-to-end 16-bit VALUE ABI parity needs
        // a follow-up beyond this stack-packing fix.
        IrType::Float(FloatWidth::F32) => (4, 4),
        IrType::Int(IntWidth::I32) => (4, 4),
        IrType::Int(IntWidth::I16) => (2, 2),
        IrType::Int(IntWidth::I8) | IrType::Bool => (1, 1),
        _ => (8, 8),
    }
}

fn classify_abi_arg(ty: &IrType, state: &mut AbiArgState) -> AbiArgLoc {
    match ty {
        IrType::Int(IntWidth::I128) => {
            if state.gp_idx + 2 <= 8 {
                let reg = state.gp_idx;
                state.gp_idx += 2;
                AbiArgLoc::GpPair(reg)
            } else {
                state.gp_idx = 8;
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        IrType::Float(FloatWidth::F64) => {
            if state.fp_idx < 8 {
                let reg = state.fp_idx;
                state.fp_idx += 1;
                AbiArgLoc::Fp(reg)
            } else {
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        IrType::Float(FloatWidth::F32) => {
            if state.fp_idx < 8 {
                let reg = state.fp_idx;
                state.fp_idx += 1;
                AbiArgLoc::Fp32(reg)
            } else {
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        IrType::Int(IntWidth::I8)
        | IrType::Int(IntWidth::I16)
        | IrType::Int(IntWidth::I32)
        | IrType::Bool => {
            if state.gp_idx < 8 {
                let reg = state.gp_idx;
                state.gp_idx += 1;
                AbiArgLoc::Gp32(reg)
            } else {
                state.gp_idx = 8;
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
        _ => {
            if state.gp_idx < 8 {
                let reg = state.gp_idx;
                state.gp_idx += 1;
                AbiArgLoc::Gp(reg)
            } else {
                state.gp_idx = 8;
                let (size, align) = abi_stack_layout(ty);
                let offset = align_to(state.stack_offset, align);
                state.stack_offset = offset + size;
                AbiArgLoc::Stack(offset)
            }
        }
    }
}

/// Select machine instructions for one IR function.
pub fn select_function(func: &Function) -> MachineFunction {
    let mut mf = MachineFunction::new(func.name.clone());
    mf.internal_only = func.internal_only;
    let mut ctx = ISelCtx::new();

    // Phase 1: allocate stack slots for all IR alloca instructions.
    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::Alloca(ty) = &inst.kind {
                let size = alloca_size(ty);
                let offset = mf.alloc_local(size);
                ctx.alloca_offsets.insert(inst.id, offset);
            }
        }
    }

    // Phase 2: create machine blocks corresponding to IR blocks.
    // Entry block already exists as MBlockId(0).
    //
    // Block labels are prefixed with the function name so two
    // functions in the same .s file don't collide on common names
    // like `do_check_1`. The `L` prefix turns them into local
    // symbols on Apple's assembler.
    ctx.block_map.insert(func.entry, MBlockId(0));
    for block in &func.blocks {
        if block.id != func.entry {
            let label = format!("L{}_{}", mf.name, block.name);
            let mb_id = mf.new_block(&label);
            ctx.block_map.insert(block.id, mb_id);
        }
    }

    enum IncomingParam {
        Narrow(VRegId, RegClass, AbiArgLoc, IrType),
        Wide(i32, AbiArgLoc),
    }

    // Phase 2.5: handle incoming parameters.
    // Create a vreg or a wide stack slot for each param.
    // The physical register save happens after the prologue.
    let mut param_info: Vec<IncomingParam> = Vec::new();
    let mut abi_state = AbiArgState::default();
    for param in &func.params {
        let loc = classify_abi_arg(&param.ty, &mut abi_state);
        if matches!(param.ty, IrType::Int(IntWidth::I128)) {
            let offset = mf.alloc_local(16);
            ctx.wide_value_slots.insert(param.id, offset);
            param_info.push(IncomingParam::Wide(offset, loc));
            continue;
        }
        let class = type_to_reg_class(&param.ty);
        let vreg = mf.new_vreg(class);
        ctx.value_map.insert(param.id, vreg);
        param_info.push(IncomingParam::Narrow(vreg, class, loc, param.ty.clone()));
    }

    // Phase 3: emit prologue in entry block.
    emit_prologue(&mut mf, MBlockId(0));

    // Phase 3.5: move incoming argument registers into param vregs.
    // Dispatch by register class: GP args from x0-x7, FP args from d0-d7.
    for info in &param_info {
        match info {
            IncomingParam::Wide(offset, AbiArgLoc::GpPair(reg)) => {
                emit_store_phys_i128_pair(
                    &mut mf,
                    MBlockId(0),
                    MachineOperand::PhysReg(PhysReg::FP),
                    *offset as i64,
                    PhysReg::Gp(*reg),
                    PhysReg::Gp(*reg + 1),
                );
            }
            IncomingParam::Wide(offset, AbiArgLoc::Stack(stack_offset)) => {
                emit_load_phys_i128_pair(
                    &mut mf,
                    MBlockId(0),
                    MachineOperand::PhysReg(PhysReg::FP),
                    16 + *stack_offset,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                emit_store_phys_i128_pair(
                    &mut mf,
                    MBlockId(0),
                    MachineOperand::PhysReg(PhysReg::FP),
                    *offset as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
            }
            IncomingParam::Narrow(vreg, RegClass::Fp64, AbiArgLoc::Fp(reg), _) => {
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::FmovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Fp(*reg)),
                    ],
                    def: Some(*vreg),
                });
            }
            IncomingParam::Narrow(vreg, RegClass::Fp32, AbiArgLoc::Fp32(reg), _) => {
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::FmovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Fp32(*reg)),
                    ],
                    def: Some(*vreg),
                });
            }
            IncomingParam::Narrow(vreg, RegClass::Gp32, AbiArgLoc::Gp32(reg), _) => {
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Gp32(*reg)),
                    ],
                    def: Some(*vreg),
                });
            }
            IncomingParam::Narrow(vreg, _, AbiArgLoc::Gp(reg), _) => {
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Gp(*reg)),
                    ],
                    def: Some(*vreg),
                });
            }
            IncomingParam::Narrow(vreg, class, AbiArgLoc::Stack(stack_offset), ty) => {
                emit_load_stack_arg_into_vreg(
                    &mut mf,
                    MBlockId(0),
                    *vreg,
                    *class,
                    ty,
                    16 + *stack_offset,
                );
            }
            IncomingParam::Wide(_, other) => {
                panic!(
                    "isel: unexpected ABI loc {:?} for incoming i128 param",
                    other
                );
            }
            IncomingParam::Narrow(_, class, other, _) => {
                panic!(
                    "isel: unexpected ABI loc {:?} for incoming {:?} param",
                    other, class
                );
            }
        }
    }

    // Phase 4a: allocate vregs for EVERY block parameter AND every
    // instruction result *before* walking any instructions. We need
    // this upfront because:
    //
    //  - A branch terminator needs to know the target block's
    //    param vregs to emit "move branch arg → target param"
    //    copies, and the target block may not have been walked yet.
    //
    //  - An instruction in block A may reference an SSA value
    //    defined in block B that appears later in `func.blocks`
    //    vec order (perfectly legal under SSA dominance — block B
    //    can dominate block A even if it comes later in the vec).
    //    Without upfront allocation, the lookup fails.
    //
    // Allocation here doesn't emit machine instructions; it just
    // reserves vreg IDs for every IR ValueId so Phase 4b can use
    // `lookup_vreg` without ordering concerns.
    for block in &func.blocks {
        for bp in &block.params {
            if matches!(bp.ty, IrType::Int(IntWidth::I128)) {
                let offset = mf.alloc_local(16);
                ctx.wide_value_slots.insert(bp.id, offset);
                continue;
            }
            let class = type_to_reg_class(&bp.ty);
            let vreg = mf.new_vreg(class);
            ctx.value_map.insert(bp.id, vreg);
        }
        for inst in &block.insts {
            // Allocas already have their backing stack slots from
            // Phase 1, but the SSA value they produce is still a real
            // pointer that later blocks may pass to calls or branch
            // params before the defining block is selected.
            //
            // Reserve the vreg here so forward-dominating alloca uses
            // are safe even when block vec order puts the use before
            // the definition.
            // Void-typed insts (Store, RuntimeCall returning void,
            // etc.) don't produce a usable value.
            if matches!(inst.ty, IrType::Void) {
                continue;
            }
            if matches!(inst.ty, IrType::Int(IntWidth::I128)) {
                let offset = mf.alloc_local(16);
                ctx.wide_value_slots.insert(inst.id, offset);
                continue;
            }
            let class = type_to_reg_class(&inst.ty);
            let vreg = mf.new_vreg(class);
            ctx.value_map.insert(inst.id, vreg);
        }
    }

    // Snapshot just each IR block's params into ctx so
    // `select_terminator` can look them up while we hold a separate
    // &mut MachineFunction borrow. We don't need a full BasicBlock
    // clone — only the param list — so this avoids cloning every
    // instruction in the function for each terminator we visit.
    for block in &func.blocks {
        ctx.block_params.insert(block.id, block.params.clone());
    }

    // Phase 4a.5: identify ICmp/FCmp → Select fusion candidates.
    //
    // An ICmp whose boolean result is used only by a single Select in
    // the same block (with no intervening flag-clobbering instruction)
    // can be fused: we suppress the CSET and pass the CMP flags
    // directly into the CSEL. This turns 4 instructions into 2:
    //
    //   CMP a, b; CSET cond, LE; CMP cond, #0; CSEL dest, tv, fv, NE
    //       →  CMP a, b; CSEL dest, tv, fv, LE
    compute_csel_fusion(func, &mut ctx);

    // Phase 4b: select instructions and terminators for each block.
    for block in &func.blocks {
        let mb_id = ctx.block_map[&block.id];

        for inst in &block.insts {
            select_inst(&mut mf, &mut ctx, mb_id, inst, func);
        }

        if let Some(term) = &block.terminator {
            select_terminator(&mut mf, &mut ctx, mb_id, term, block, func);
        }
    }

    mf
}

fn select_call_inst(
    mf: &mut MachineFunction,
    ctx: &mut ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    func: &Function,
) {
    let (label, args, runtime_func, indirect_target) = match &inst.kind {
        InstKind::Call(FuncRef::External(name), args) => {
            (name.clone(), args.as_slice(), None, None)
        }
        InstKind::Call(FuncRef::Internal(idx), args) => {
            (format!("_func_{}", idx), args.as_slice(), None, None)
        }
        InstKind::Call(FuncRef::Indirect(target), args) => {
            (String::new(), args.as_slice(), None, Some(*target))
        }
        InstKind::RuntimeCall(rf, args) => (String::new(), args.as_slice(), Some(rf), None),
        _ => unreachable!(),
    };

    let mut abi_state = AbiArgState::default();
    let mut arg_locs = Vec::with_capacity(args.len());
    for &arg_val in args {
        let arg_ty = func
            .value_type(arg_val)
            .unwrap_or_else(|| panic!("isel: missing type for call arg %{}", arg_val.0));
        arg_locs.push((arg_val, classify_abi_arg(&arg_ty, &mut abi_state), arg_ty));
    }
    let label = runtime_func
        .map(|rf| runtime_func_symbol(rf, &arg_locs))
        .unwrap_or(label);
    if abi_state.stack_offset > 0 {
        mf.reserve_outgoing_args(abi_state.stack_offset as u32);
    }

    let mut pending_reg_arg_moves: Vec<(ArmOpcode, PhysReg, VRegId)> = Vec::new();
    for (arg_val, loc, arg_ty) in arg_locs {
        if matches!(arg_ty, IrType::Int(IntWidth::I128)) {
            let arg_slot = ctx.lookup_wide_slot(arg_val);
            match loc {
                AbiArgLoc::GpPair(reg) => {
                    emit_load_phys_i128_pair(
                        mf,
                        mb,
                        MachineOperand::PhysReg(PhysReg::FP),
                        arg_slot as i64,
                        PhysReg::Gp(reg),
                        PhysReg::Gp(reg + 1),
                    );
                }
                AbiArgLoc::Stack(stack_offset) => {
                    emit_load_phys_i128_pair(
                        mf,
                        mb,
                        MachineOperand::PhysReg(PhysReg::FP),
                        arg_slot as i64,
                        PhysReg::Gp(16),
                        PhysReg::Gp(17),
                    );
                    emit_store_phys_i128_pair(
                        mf,
                        mb,
                        MachineOperand::PhysReg(PhysReg::Sp),
                        stack_offset,
                        PhysReg::Gp(16),
                        PhysReg::Gp(17),
                    );
                }
                other => {
                    panic!("isel: unexpected ABI loc {:?} for outgoing i128 arg", other);
                }
            }
            continue;
        }

        let arg_vreg = ctx.lookup_vreg(arg_val);
        let arg_class = mf.vregs.iter().find(|v| v.id == arg_vreg).map(|v| v.class);
        match (arg_class, loc) {
            (Some(RegClass::Fp64), AbiArgLoc::Fp(reg)) => {
                pending_reg_arg_moves.push((ArmOpcode::FmovReg, PhysReg::Fp(reg), arg_vreg));
            }
            (Some(RegClass::Fp32), AbiArgLoc::Fp32(reg)) => {
                pending_reg_arg_moves.push((ArmOpcode::FmovReg, PhysReg::Fp32(reg), arg_vreg));
            }
            (Some(RegClass::Gp32), AbiArgLoc::Gp32(reg)) => {
                pending_reg_arg_moves.push((ArmOpcode::MovReg, PhysReg::Gp32(reg), arg_vreg));
            }
            (Some(RegClass::Gp64), AbiArgLoc::Gp(reg)) => {
                pending_reg_arg_moves.push((ArmOpcode::MovReg, PhysReg::Gp(reg), arg_vreg));
            }
            (Some(class), AbiArgLoc::Stack(stack_offset)) => {
                emit_store_stack_arg_from_vreg(mf, mb, arg_vreg, class, &arg_ty, stack_offset);
            }
            (Some(class), other) => {
                panic!(
                    "isel: unexpected ABI loc {:?} for outgoing {:?} arg",
                    other, class
                );
            }
            (None, _) => {
                panic!("isel: call arg vreg class missing for %{}", arg_val.0);
            }
        }
    }

    for (opcode, dst, src) in pending_reg_arg_moves {
        mf.block_mut(mb).insts.push(MachineInst {
            opcode,
            operands: vec![MachineOperand::PhysReg(dst), MachineOperand::VReg(src)],
            def: None,
        });
    }

    if let Some(target) = indirect_target {
        mf.block_mut(mb).insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::VReg(ctx.lookup_vreg(target))],
            def: None,
        });
    } else {
        mf.block_mut(mb).insts.push(MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![MachineOperand::Extern(label)],
            def: None,
        });
    }

    if matches!(inst.ty, IrType::Int(IntWidth::I128)) {
        let dest_slot = ctx.lookup_wide_slot(inst.id);
        emit_store_phys_i128_pair(
            mf,
            mb,
            MachineOperand::PhysReg(PhysReg::FP),
            dest_slot as i64,
            PhysReg::Gp(0),
            PhysReg::Gp(1),
        );
    } else if inst.ty != IrType::Void {
        let class = type_to_reg_class(&inst.ty);
        let dest = ctx.get_vreg(mf, inst.id, class);
        let (src_reg, opcode) = match class {
            RegClass::Fp64 => (PhysReg::Fp(0), ArmOpcode::FmovReg),
            RegClass::Fp32 => (PhysReg::Fp32(0), ArmOpcode::FmovReg),
            RegClass::Gp32 => (PhysReg::Gp32(0), ArmOpcode::MovReg),
            RegClass::Gp64 => (PhysReg::Gp(0), ArmOpcode::MovReg),
        };
        mf.block_mut(mb).insts.push(MachineInst {
            opcode,
            operands: vec![MachineOperand::VReg(dest), MachineOperand::PhysReg(src_reg)],
            def: Some(dest),
        });
    } else {
        ctx.get_vreg(mf, inst.id, RegClass::Gp64);
    }
}

/// Instruction selection context.
struct ISelCtx {
    /// IR ValueId → MIR VRegId.
    value_map: HashMap<ValueId, VRegId>,
    /// IR wide scalar ValueId → stack slot offset used as its backing store.
    wide_value_slots: HashMap<ValueId, i32>,
    /// IR BlockId → MIR MBlockId.
    block_map: HashMap<BlockId, MBlockId>,
    /// IR alloca ValueId → stack frame offset.
    alloca_offsets: HashMap<ValueId, i32>,
    /// IR BlockId → its block params. Snapshotted before phase 4b
    /// so terminator selection can read each target's params
    /// without re-borrowing the function while &mut MachineFunction
    /// is held. Cloning just the param vec is dramatically cheaper
    /// than cloning the whole BasicBlock — instructions can be in
    /// the thousands, params are typically 0-3.
    block_params: HashMap<BlockId, Vec<BlockParam>>,
    /// ICmp/FCmp ValueIds that are exclusively consumed by a Select in
    /// the same block with no intervening flag-clobbering instruction.
    /// For these, we suppress CSET during ICmp lowering and use the
    /// flags directly from the CMP in the CSEL.
    select_fused: HashSet<ValueId>,
    /// For each fused ICmp/FCmp, the ARM condition code to use in the
    /// CSEL (determined at the time we suppress the CSET).
    fused_arm_cond: HashMap<ValueId, ArmCond>,
}

impl ISelCtx {
    fn new() -> Self {
        Self {
            value_map: HashMap::new(),
            wide_value_slots: HashMap::new(),
            block_map: HashMap::new(),
            alloca_offsets: HashMap::new(),
            block_params: HashMap::new(),
            select_fused: HashSet::new(),
            fused_arm_cond: HashMap::new(),
        }
    }

    /// Get the vreg for an IR value, or create one if needed.
    /// In debug builds, asserts that an existing mapping has the
    /// same register class as requested — a class mismatch means
    /// Phase 4a (vreg pre-allocation) and Phase 4b (instruction
    /// selection) disagree about a value's type, which would
    /// silently corrupt code.
    fn get_vreg(&mut self, mf: &mut MachineFunction, val: ValueId, class: RegClass) -> VRegId {
        if let Some(&vreg) = self.value_map.get(&val) {
            debug_assert!(
                mf.vregs.iter().find(|v| v.id == vreg).map(|v| v.class) == Some(class),
                "isel: vreg class mismatch for IR value %{} (existing class \
                 differs from requested {:?}) — phase 4a/4b disagreement",
                val.0,
                class,
            );
            return vreg;
        }
        let vreg = mf.new_vreg(class);
        self.value_map.insert(val, vreg);
        vreg
    }

    /// Get the vreg for an IR value, assuming it was already mapped.
    fn lookup_vreg(&self, val: ValueId) -> VRegId {
        *self.value_map.get(&val).unwrap_or_else(|| {
            panic!(
                "isel: unmapped IR value %{} — phase 4a should have allocated \
                 a vreg for every IR value before phase 4b runs. {} values are \
                 currently mapped. This usually means a forward reference, \
                 a missing block param, or a value defined in an unreachable \
                 block.",
                val.0,
                self.value_map.len(),
            )
        })
    }

    /// Get machine block for an IR block.
    fn lookup_block(&self, block: BlockId) -> MBlockId {
        *self.block_map.get(&block).unwrap_or(&MBlockId(0))
    }

    fn lookup_wide_slot(&self, val: ValueId) -> i32 {
        *self.wide_value_slots.get(&val).unwrap_or_else(|| {
            panic!(
                "isel: unmapped wide i128 value %{} — phase 4a should have allocated \
                 a backing slot for every supported i128 SSA value before phase 4b runs",
                val.0
            )
        })
    }
}

/// Select machine instructions for a single IR instruction.
fn select_inst(
    mf: &mut MachineFunction,
    ctx: &mut ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    func: &Function,
) {
    if matches!(inst.ty, IrType::Int(IntWidth::I128)) {
        match &inst.kind {
            InstKind::ConstInt(val, IntWidth::I128) => {
                let dest_slot = ctx.lookup_wide_slot(inst.id);
                emit_const_i128_to_phys_pair(mf, mb, *val, PhysReg::Gp(16), PhysReg::Gp(17));
                emit_store_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    dest_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                return;
            }
            InstKind::Undef(_) => {
                let dest_slot = ctx.lookup_wide_slot(inst.id);
                emit_const_i128_to_phys_pair(mf, mb, 0, PhysReg::Gp(16), PhysReg::Gp(17));
                emit_store_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    dest_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                return;
            }
            InstKind::IAdd(a, b) => {
                emit_i128_binop_via_slots(mf, ctx, mb, I128BinOp::Add, inst.id, *a, *b);
                return;
            }
            InstKind::ISub(a, b) => {
                emit_i128_binop_via_slots(mf, ctx, mb, I128BinOp::Sub, inst.id, *a, *b);
                return;
            }
            InstKind::INeg(a) => {
                let dest_slot = ctx.lookup_wide_slot(inst.id);
                let src_slot = ctx.lookup_wide_slot(*a);
                emit_load_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    src_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                emit_i128_neg(mf, mb, PhysReg::Gp(16), PhysReg::Gp(17));
                emit_store_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    dest_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                return;
            }
            InstKind::Load(addr) => {
                let dest_slot = ctx.lookup_wide_slot(inst.id);
                if let Some(&offset) = ctx.alloca_offsets.get(addr) {
                    emit_load_phys_i128_pair(
                        mf,
                        mb,
                        MachineOperand::PhysReg(PhysReg::FP),
                        offset as i64,
                        PhysReg::Gp(16),
                        PhysReg::Gp(17),
                    );
                } else {
                    let base = ctx.lookup_vreg(*addr);
                    emit_load_phys_i128_pair(
                        mf,
                        mb,
                        MachineOperand::VReg(base),
                        0,
                        PhysReg::Gp(16),
                        PhysReg::Gp(17),
                    );
                }
                emit_store_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    dest_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                return;
            }
            InstKind::Select(cond, tv, fv) => {
                let arm_cond = if let Some(&fused_cond) = ctx.fused_arm_cond.get(cond) {
                    fused_cond
                } else {
                    let cond_reg = ctx.lookup_vreg(*cond);
                    mf.block_mut(mb).insts.push(MachineInst {
                        opcode: ArmOpcode::CmpImm,
                        operands: vec![MachineOperand::VReg(cond_reg), MachineOperand::Imm(0)],
                        def: None,
                    });
                    ArmCond::Ne
                };
                let dest_slot = ctx.lookup_wide_slot(inst.id);
                let true_slot = ctx.lookup_wide_slot(*tv);
                let false_slot = ctx.lookup_wide_slot(*fv);
                emit_load_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    true_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                emit_load_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    false_slot as i64,
                    PhysReg::Gp(8),
                    PhysReg::Gp(9),
                );
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::CselReg,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(16)),
                        MachineOperand::PhysReg(PhysReg::Gp(16)),
                        MachineOperand::PhysReg(PhysReg::Gp(8)),
                        MachineOperand::Cond(arm_cond),
                    ],
                    def: None,
                });
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::CselReg,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(17)),
                        MachineOperand::PhysReg(PhysReg::Gp(17)),
                        MachineOperand::PhysReg(PhysReg::Gp(9)),
                        MachineOperand::Cond(arm_cond),
                    ],
                    def: None,
                });
                emit_store_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    dest_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                return;
            }
            InstKind::Call(..) => {
                select_call_inst(mf, ctx, mb, inst, func);
                return;
            }
            _ => {
                panic!(
                    "isel: unsupported i128 instruction reached backend despite gating: {:?}",
                    inst.kind
                );
            }
        }
    }

    match &inst.kind {
        // ---- Constants ----
        InstKind::ConstInt(val, width) => {
            let class = int_width_class(width);
            let dest = ctx.get_vreg(mf, inst.id, class);
            emit_const_int(mf, mb, dest, *val, *width);
        }

        InstKind::ConstFloat(val, width) => {
            let class = float_width_class(width);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let cp_idx = match width {
                FloatWidth::F32 => mf.add_const(ConstPoolEntry::F32(*val as f32)),
                FloatWidth::F64 => mf.add_const(ConstPoolEntry::F64(*val)),
            };
            // ADRP + LDR from constant pool.
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::AdrpLdr,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::ConstPool(cp_idx),
                ],
                def: Some(dest),
            });
        }

        InstKind::ConstBool(val) => {
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp32);
            emit_const_int(mf, mb, dest, if *val { 1 } else { 0 }, IntWidth::I32);
        }

        InstKind::ConstString(bytes) => {
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp64);
            let cp_idx = mf.add_const(ConstPoolEntry::Bytes(bytes.clone()));
            // Use ADRP+ADD to compute the address (not ADRP+LDR which loads the value).
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::AdrpAdd,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::ConstPool(cp_idx),
                ],
                def: Some(dest),
            });
        }

        InstKind::Undef(_) => {
            // Emit a deterministic zero instead of leaving the vreg
            // undefined. A truly undefined vreg lets the register
            // allocator hand us whatever physical register is free,
            // and that register's stale contents leak into reads —
            // which makes optimization-level diffs nondeterministic
            // and turns "undef ⇒ anything" into "undef ⇒ whatever
            // happened to be in x14 at this point in the program."
            //
            // mem2reg synthesizes Undef as the initial value of a
            // promoted slot before any store. The Fortran semantics
            // for reading uninitialized storage are undefined, but
            // a hard zero is at least reproducible across opt
            // levels and friendly to debuggers.
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            match class {
                RegClass::Gp32 => {
                    mf.block_mut(mb).insts.push(MachineInst {
                        opcode: ArmOpcode::MovReg,
                        operands: vec![
                            MachineOperand::VReg(dest),
                            MachineOperand::PhysReg(PhysReg::Wzr),
                        ],
                        def: Some(dest),
                    });
                }
                RegClass::Gp64 => {
                    mf.block_mut(mb).insts.push(MachineInst {
                        opcode: ArmOpcode::MovReg,
                        operands: vec![
                            MachineOperand::VReg(dest),
                            MachineOperand::PhysReg(PhysReg::Xzr),
                        ],
                        def: Some(dest),
                    });
                }
                RegClass::Fp32 => {
                    let cp_idx = mf.add_const(ConstPoolEntry::F32(0.0));
                    mf.block_mut(mb).insts.push(MachineInst {
                        opcode: ArmOpcode::AdrpLdr,
                        operands: vec![
                            MachineOperand::VReg(dest),
                            MachineOperand::ConstPool(cp_idx),
                        ],
                        def: Some(dest),
                    });
                }
                RegClass::Fp64 => {
                    let cp_idx = mf.add_const(ConstPoolEntry::F64(0.0));
                    mf.block_mut(mb).insts.push(MachineInst {
                        opcode: ArmOpcode::AdrpLdr,
                        operands: vec![
                            MachineOperand::VReg(dest),
                            MachineOperand::ConstPool(cp_idx),
                        ],
                        def: Some(dest),
                    });
                }
            }
        }

        // ---- Integer arithmetic ----
        InstKind::IAdd(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::AddReg, *a, *b),
        InstKind::ISub(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::SubReg, *a, *b),
        InstKind::IMul(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::Mul, *a, *b),
        InstKind::IDiv(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::Sdiv, *a, *b),
        InstKind::IMod(a, b) => {
            // imod = a - (a / b) * b → SDIV + MSUB
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let tmp = mf.new_vreg(class);
            // tmp = sdiv a, b
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Sdiv,
                operands: vec![
                    MachineOperand::VReg(tmp),
                    MachineOperand::VReg(va),
                    MachineOperand::VReg(vb),
                ],
                def: Some(tmp),
            });
            // dest = msub tmp, vb, va → va - tmp * vb = a - (a/b)*b
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Msub,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::VReg(tmp),
                    MachineOperand::VReg(vb),
                    MachineOperand::VReg(va),
                ],
                def: Some(dest),
            });
        }
        InstKind::INeg(a) => {
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let va = ctx.lookup_vreg(*a);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Neg,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(va)],
                def: Some(dest),
            });
        }

        // ---- Float arithmetic ----
        InstKind::FAdd(a, b) => emit_float_binop(
            mf,
            ctx,
            mb,
            inst,
            &inst.ty,
            *a,
            *b,
            ArmOpcode::FaddS,
            ArmOpcode::FaddD,
        ),
        InstKind::FSub(a, b) => emit_float_binop(
            mf,
            ctx,
            mb,
            inst,
            &inst.ty,
            *a,
            *b,
            ArmOpcode::FsubS,
            ArmOpcode::FsubD,
        ),
        InstKind::FMul(a, b) => emit_float_binop(
            mf,
            ctx,
            mb,
            inst,
            &inst.ty,
            *a,
            *b,
            ArmOpcode::FmulS,
            ArmOpcode::FmulD,
        ),
        InstKind::FDiv(a, b) => emit_float_binop(
            mf,
            ctx,
            mb,
            inst,
            &inst.ty,
            *a,
            *b,
            ArmOpcode::FdivS,
            ArmOpcode::FdivD,
        ),
        InstKind::FNeg(a) => {
            let (class, opcode) = match &inst.ty {
                IrType::Float(FloatWidth::F32) => (RegClass::Fp32, ArmOpcode::FnegS),
                _ => (RegClass::Fp64, ArmOpcode::FnegD),
            };
            let dest = ctx.get_vreg(mf, inst.id, class);
            let va = ctx.lookup_vreg(*a);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(va)],
                def: Some(dest),
            });
        }
        InstKind::FPow(a, b) => {
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            let (func_name, arg0, arg1, ret) = match &inst.ty {
                IrType::Float(FloatWidth::F32) => {
                    ("powf", PhysReg::Fp32(0), PhysReg::Fp32(1), PhysReg::Fp32(0))
                }
                _ => ("pow", PhysReg::Fp(0), PhysReg::Fp(1), PhysReg::Fp(0)),
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::FmovReg,
                operands: vec![MachineOperand::PhysReg(arg0), MachineOperand::VReg(va)],
                def: None,
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::FmovReg,
                operands: vec![MachineOperand::PhysReg(arg1), MachineOperand::VReg(vb)],
                def: None,
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Bl,
                operands: vec![MachineOperand::Extern(func_name.into())],
                def: None,
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::FmovReg,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::PhysReg(ret)],
                def: Some(dest),
            });
        }

        // ---- Comparisons ----
        InstKind::ICmp(op, a, b) => {
            if matches!(func.value_type(*a), Some(IrType::Int(IntWidth::I128)))
                || matches!(func.value_type(*b), Some(IrType::Int(IntWidth::I128)))
            {
                let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp32);
                let lhs_slot = ctx.lookup_wide_slot(*a);
                let rhs_slot = ctx.lookup_wide_slot(*b);
                emit_load_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    lhs_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                emit_load_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    rhs_slot as i64,
                    PhysReg::Gp(8),
                    PhysReg::Gp(9),
                );
                match op {
                    CmpOp::Eq | CmpOp::Ne => {
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::CmpReg,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp(16)),
                                MachineOperand::PhysReg(PhysReg::Gp(8)),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::Cset,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(10)),
                                MachineOperand::Cond(cmp_to_arm_cond(*op)),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::CmpReg,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp(17)),
                                MachineOperand::PhysReg(PhysReg::Gp(9)),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::Cset,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(11)),
                                MachineOperand::Cond(cmp_to_arm_cond(*op)),
                            ],
                            def: None,
                        });
                        let combine = match op {
                            CmpOp::Eq => ArmOpcode::AndReg,
                            CmpOp::Ne => ArmOpcode::OrrReg,
                            _ => unreachable!(),
                        };
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: combine,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(10)),
                                MachineOperand::PhysReg(PhysReg::Gp32(10)),
                                MachineOperand::PhysReg(PhysReg::Gp32(11)),
                            ],
                            def: None,
                        });
                    }
                    CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => {
                        let (hi_cond, lo_cond) = i128_ordered_conds(*op);
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::CmpReg,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp(17)),
                                MachineOperand::PhysReg(PhysReg::Gp(9)),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::Cset,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(10)),
                                MachineOperand::Cond(hi_cond),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::Cset,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(11)),
                                MachineOperand::Cond(ArmCond::Eq),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::CmpReg,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp(16)),
                                MachineOperand::PhysReg(PhysReg::Gp(8)),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::Cset,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(8)),
                                MachineOperand::Cond(lo_cond),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::AndReg,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(11)),
                                MachineOperand::PhysReg(PhysReg::Gp32(11)),
                                MachineOperand::PhysReg(PhysReg::Gp32(8)),
                            ],
                            def: None,
                        });
                        mf.block_mut(mb).insts.push(MachineInst {
                            opcode: ArmOpcode::OrrReg,
                            operands: vec![
                                MachineOperand::PhysReg(PhysReg::Gp32(10)),
                                MachineOperand::PhysReg(PhysReg::Gp32(10)),
                                MachineOperand::PhysReg(PhysReg::Gp32(11)),
                            ],
                            def: None,
                        });
                    }
                }
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::PhysReg(PhysReg::Gp32(10)),
                    ],
                    def: Some(dest),
                });
                return;
            }

            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp32);
            let va = icmp_operand_vreg(mf, ctx, mb, func, *a, *b);
            let vb = icmp_operand_vreg(mf, ctx, mb, func, *b, *a);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::CmpReg,
                operands: vec![MachineOperand::VReg(va), MachineOperand::VReg(vb)],
                def: None,
            });
            // If this ICmp feeds exclusively into a Select (detected in the
            // pre-pass), suppress CSET. The Select will use the flags directly.
            if !ctx.select_fused.contains(&inst.id) {
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::Cset,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::Cond(cmp_to_arm_cond(*op)),
                    ],
                    def: Some(dest),
                });
            }
        }
        InstKind::FCmp(op, a, b) => {
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp32);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::FCmpReg,
                operands: vec![MachineOperand::VReg(va), MachineOperand::VReg(vb)],
                def: None,
            });
            if !ctx.select_fused.contains(&inst.id) {
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::FCset,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::Cond(fcmp_to_arm_cond(*op)),
                    ],
                    def: Some(dest),
                });
            }
        }

        // ---- Logic ----
        InstKind::And(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::AndReg, *a, *b),
        InstKind::Or(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::OrrReg, *a, *b),
        InstKind::Not(a) => {
            // Logical NOT: CMP src, #0; CSET dest, EQ
            // If src == 0 (false), EQ is true → dest = 1 (true).
            // If src != 0 (true), EQ is false → dest = 0 (false).
            // This correctly handles any truthy value, not just 0/1.
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp32);
            let va = ctx.lookup_vreg(*a);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::CmpImm,
                operands: vec![MachineOperand::VReg(va), MachineOperand::Imm(0)],
                def: None,
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Cset,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::Cond(ArmCond::Eq),
                ],
                def: Some(dest),
            });
        }

        // ---- Select (CSEL) ----
        //
        // Fast path: if the condition was produced by an ICmp/FCmp in the
        // same block with no other users, the pre-pass marked it as fused.
        // We already emitted `CMP a, b` (no CSET), so the flags are live.
        // Use them directly: `CSEL dest, tv, fv, <arm_cond>`.
        //
        // Slow path (unfused): the condition is an arbitrary boolean in a
        // register. Materialize with `CMP cond, #0; CSEL dest, tv, fv, NE`.
        InstKind::Select(cond, tv, fv) => {
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let true_reg = coerce_select_operand_vreg(mf, ctx, mb, func, *tv, &inst.ty);
            let false_reg = coerce_select_operand_vreg(mf, ctx, mb, func, *fv, &inst.ty);

            let arm_cond = if let Some(&fused_cond) = ctx.fused_arm_cond.get(cond) {
                // Flags already set by the fused CMP — no extra compare needed.
                fused_cond
            } else {
                // Unfused: compare the boolean register against 0.
                let cond_reg = ctx.lookup_vreg(*cond);
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::CmpImm,
                    operands: vec![MachineOperand::VReg(cond_reg), MachineOperand::Imm(0)],
                    def: None,
                });
                ArmCond::Ne
            };

            let opcode = if class == RegClass::Fp32 || class == RegClass::Fp64 {
                ArmOpcode::FcselReg
            } else {
                ArmOpcode::CselReg
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::VReg(true_reg),
                    MachineOperand::VReg(false_reg),
                    MachineOperand::Cond(arm_cond),
                ],
                def: Some(dest),
            });
        }

        // ---- Float: fabs, fsqrt ----
        InstKind::FAbs(a) => {
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let opcode = if class == RegClass::Fp64 {
                ArmOpcode::FabsD
            } else {
                ArmOpcode::FabsS
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }
        InstKind::FSqrt(a) => {
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let opcode = if class == RegClass::Fp64 {
                ArmOpcode::FsqrtD
            } else {
                ArmOpcode::FsqrtS
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }

        // ---- Bitwise ----
        InstKind::BitAnd(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::AndReg, *a, *b),
        InstKind::BitOr(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::OrrReg, *a, *b),
        InstKind::BitXor(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::EorReg, *a, *b),
        InstKind::BitNot(a) => {
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Mvn,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }
        InstKind::Shl(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::LslReg, *a, *b),
        InstKind::LShr(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::LsrReg, *a, *b),
        InstKind::AShr(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::AsrReg, *a, *b),
        InstKind::CountLeadingZeros(a) => {
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Clz,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }
        InstKind::CountTrailingZeros(a) => {
            // CTZ = CLZ(RBIT(x))
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let tmp = mf.new_vreg(class);
            let dest = ctx.get_vreg(mf, inst.id, class);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Rbit,
                operands: vec![MachineOperand::VReg(tmp), MachineOperand::VReg(src)],
                def: Some(tmp),
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Clz,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(tmp)],
                def: Some(dest),
            });
        }
        InstKind::PopCount(a) => {
            // ARM64 popcount: FMOV Vd.8B, Xn; CNT Vd.8B, Vd.8B; ADDV Bd, Vd.8B; FMOV Wd, Sd
            // For simplicity, emit a runtime call.
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            // Placeholder: use CLZ-based Hamming weight or runtime call.
            // For now, move src to dest (will be replaced with proper popcount later).
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }

        // ---- Conversions ----
        InstKind::IntToFloat(a, fw) => {
            let src = ctx.lookup_vreg(*a);
            let src_class = mf.vregs.iter().find(|v| v.id == src).map(|v| v.class);
            let is_64bit_src = matches!(src_class, Some(RegClass::Gp64));
            let (class, opcode) = match (fw, is_64bit_src) {
                (FloatWidth::F32, false) => (RegClass::Fp32, ArmOpcode::ScvtfSW),
                (FloatWidth::F32, true) => (RegClass::Fp32, ArmOpcode::ScvtfSX),
                (FloatWidth::F64, false) => (RegClass::Fp64, ArmOpcode::ScvtfDW),
                (FloatWidth::F64, true) => (RegClass::Fp64, ArmOpcode::ScvtfDX),
            };
            let dest = ctx.get_vreg(mf, inst.id, class);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }
        InstKind::FloatToInt(a, iw) => {
            let src = ctx.lookup_vreg(*a);
            let src_class = mf.vregs.iter().find(|v| v.id == src).map(|v| v.class);
            let is_f64_src = matches!(src_class, Some(RegClass::Fp64));
            let is_64bit_dest = matches!(iw, IntWidth::I64);
            let class = int_width_class(iw);
            let opcode = match (is_64bit_dest, is_f64_src) {
                (false, false) => ArmOpcode::FcvtzsWS,
                (false, true) => ArmOpcode::FcvtzsWD,
                (true, false) => ArmOpcode::FcvtzsXS,
                (true, true) => ArmOpcode::FcvtzsXD,
            };
            let dest = ctx.get_vreg(mf, inst.id, class);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }
        InstKind::FloatExtend(a, _) => {
            let src = ctx.lookup_vreg(*a);
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Fp64);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::FcvtDS,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }
        InstKind::FloatTrunc(a, _) => {
            let src = ctx.lookup_vreg(*a);
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Fp32);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::FcvtSD,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }

        // ---- Memory ----
        InstKind::GlobalAddr(name) => {
            // Materialize the address of a module-level global into
            // a Gp64 vreg via ADRP+ADD against `_globalname`. Loads
            // and stores then operate on this pointer the same way
            // they operate on an alloca address.
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp64);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::AdrpAdd,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::GlobalLabel(name.clone()),
                ],
                def: Some(dest),
            });
        }

        InstKind::Alloca(_) => {
            // Alloca is handled in Phase 1 (stack slot allocation).
            // The "address" is a frame slot offset. Map the ValueId to the offset.
            if let Some(&offset) = ctx.alloca_offsets.get(&inst.id) {
                let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp64);
                // Materialize address: SUB dest, FP, #abs(offset)
                // Offsets are negative from FP, so we subtract the absolute value.
                let abs_offset = (-offset) as i64;
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::SubImm,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(abs_offset),
                    ],
                    def: Some(dest),
                });
            }
        }

        InstKind::Load(addr) => {
            // Audit CRITICAL-2: dispatch on the IR result type so the
            // load opcode width matches the value, not the pointer.
            // Previously every integer load used `ldr w_, [_]` regardless
            // of width, silently reading 4 bytes for an i8 load.
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let opcode = load_opcode_for(&inst.ty, class);
            let (base_op, offset_op) = narrow_load_store_addr(ctx, *addr);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::VReg(dest), base_op, offset_op],
                def: Some(dest),
            });
        }

        InstKind::Store(val, addr) => {
            if matches!(func.value_type(*val), Some(IrType::Int(IntWidth::I128))) {
                let src_slot = ctx.lookup_wide_slot(*val);
                emit_load_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    src_slot as i64,
                    PhysReg::Gp(16),
                    PhysReg::Gp(17),
                );
                if let Some(&offset) = ctx.alloca_offsets.get(addr) {
                    emit_store_phys_i128_pair(
                        mf,
                        mb,
                        MachineOperand::PhysReg(PhysReg::FP),
                        offset as i64,
                        PhysReg::Gp(16),
                        PhysReg::Gp(17),
                    );
                } else {
                    let base = ctx.lookup_vreg(*addr);
                    emit_store_phys_i128_pair(
                        mf,
                        mb,
                        MachineOperand::VReg(base),
                        0,
                        PhysReg::Gp(16),
                        PhysReg::Gp(17),
                    );
                }
                return;
            }

            let val_vreg = ctx.lookup_vreg(*val);
            // Audit CRITICAL-2: dispatch on the *value*'s declared IR
            // type, not the pointer's pointee — byte-level GEPs into
            // derived types and array constructors reuse `Ptr<i8>` as a
            // generic offset cursor, so dispatching by the pointee
            // would silently truncate non-byte stores.
            let val_ty = func.value_type(*val);
            let val_class = mf.vregs
                .iter()
                .find(|v| v.id == val_vreg)
                .map(|v| v.class)
                .unwrap_or(RegClass::Gp64);
            let opcode = store_opcode_for(val_ty.as_ref(), val_class);
            let (base_op, offset_op) = narrow_load_store_addr(ctx, *addr);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::VReg(val_vreg), base_op, offset_op],
                def: None,
            });
        }

        InstKind::GetElementPtr(base, indices) => {
            // GEP: base + index * elem_size
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp64);
            let base_src = ctx.lookup_vreg(*base);
            let base_vreg = if mf.vregs.iter().find(|v| v.id == base_src).map(|v| v.class)
                != Some(RegClass::Gp64)
            {
                let widened = mf.new_vreg(RegClass::Gp64);
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::VReg(widened),
                        MachineOperand::VReg(base_src),
                    ],
                    def: Some(widened),
                });
                widened
            } else {
                base_src
            };

            // Determine element size from the GEP result type (Ptr<elem_ty>).
            // Scalar IR bool values are lowered as byte-sized SSA values, but
            // Fortran LOGICAL storage still uses the default 4-byte kind in
            // stack slots and arrays.
            let elem_size = match &inst.ty {
                IrType::Ptr(inner) => match inner.as_ref() {
                    IrType::Struct(_) => alloca_size(inner) as i64,
                    IrType::Bool => 4,
                    _ => inner.size_bytes() as i64,
                },
                _ => 4, // fallback
            };

            if let Some(idx) = indices.first() {
                let idx_src = ctx.lookup_vreg(*idx);
                let idx_vreg = if mf.vregs.iter().find(|v| v.id == idx_src).map(|v| v.class)
                    == Some(RegClass::Gp64)
                {
                    idx_src
                } else {
                    let widened = mf.new_vreg(RegClass::Gp64);
                    let opcode = if matches!(func.value_type(*idx), Some(IrType::Bool)) {
                        ArmOpcode::MovReg
                    } else {
                        ArmOpcode::Sxtw
                    };
                    mf.block_mut(mb).insts.push(MachineInst {
                        opcode,
                        operands: vec![
                            MachineOperand::VReg(widened),
                            MachineOperand::VReg(idx_src),
                        ],
                        def: Some(widened),
                    });
                    widened
                };
                let tmp = mf.new_vreg(RegClass::Gp64);
                emit_const_int(mf, mb, tmp, elem_size as i128, IntWidth::I64);
                let scaled = mf.new_vreg(RegClass::Gp64);
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::Mul,
                    operands: vec![
                        MachineOperand::VReg(scaled),
                        MachineOperand::VReg(idx_vreg),
                        MachineOperand::VReg(tmp),
                    ],
                    def: Some(scaled),
                });
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::AddReg,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::VReg(base_vreg),
                        MachineOperand::VReg(scaled),
                    ],
                    def: Some(dest),
                });
            } else {
                // No indices — just copy the base.
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(base_vreg)],
                    def: Some(dest),
                });
            }
        }

        // ---- Calls ----
        InstKind::Call(..) | InstKind::RuntimeCall(..) => {
            select_call_inst(mf, ctx, mb, inst, func);
        }

        // ---- Integer extend/truncate ----
        InstKind::IntExtend(a, _target_width, signed) => {
            let src = ctx.lookup_vreg(*a);
            // Pick the opcode based on the SOURCE width, not the
            // target. ARM64 has distinct SXTB/SXTH/SXTW instructions
            // for 8/16/32-bit sources; using SXTW on anything other
            // than a 32-bit source (or with a non-X dest) yields
            // "invalid operand for instruction" at the assembler.
            let src_ty = func.value_type(*a);
            let src_width = match src_ty.as_ref() {
                Some(IrType::Int(IntWidth::I8)) => 8,
                Some(IrType::Int(IntWidth::I16)) => 16,
                Some(IrType::Int(IntWidth::I32)) | Some(IrType::Bool) => 32,
                Some(IrType::Int(IntWidth::I64)) => 64,
                _ => 32, // conservative default
            };
            let dest_width = match &inst.ty {
                IrType::Int(IntWidth::I8)
                | IrType::Int(IntWidth::I16)
                | IrType::Int(IntWidth::I32)
                | IrType::Bool => 32,
                IrType::Int(IntWidth::I64) => 64,
                _ => 32,
            };
            // Dest register class follows the declared target
            // bit-width, with one exception: SXTW requires an
            // X-register destination, so promote to Gp64 when
            // source is 32 AND target is 64.
            let dest_class = if dest_width == 64 {
                RegClass::Gp64
            } else {
                RegClass::Gp32
            };
            let dest = ctx.get_vreg(mf, inst.id, dest_class);

            if !*signed {
                // Zero-extend: MOV (ARM64 implicitly zero-extends W→X).
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                    def: Some(dest),
                });
            } else if src_width >= dest_width {
                // Same-width or wider source (bogus from lowering's
                // perspective but observed in practice when a
                // function-result intrinsic mis-resolves). Emit MOV
                // rather than an illegal SXTW Wd, Wn.
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                    def: Some(dest),
                });
            } else {
                let opcode = match src_width {
                    8 => ArmOpcode::Sxtb,
                    16 => ArmOpcode::Sxth,
                    32 => ArmOpcode::Sxtw,
                    _ => ArmOpcode::MovReg, // unreachable given the bool above
                };
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode,
                    operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                    def: Some(dest),
                });
            }
        }

        InstKind::IntTrunc(a, _) => {
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            // Truncate: just MOV — the 32-bit register naturally truncates.
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }

        InstKind::PtrToInt(a) => {
            // Pointer is already an i64 in a GP register — just mov.
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }

        InstKind::IntToPtr(a, _) => {
            // Integer already in a GP register — treat as pointer via mov.
            let src = ctx.lookup_vreg(*a);
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp64);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
                def: Some(dest),
            });
        }

        // Remaining: ExtractField, InsertField — placeholder.
        _ => {
            let class = type_to_reg_class(&inst.ty);
            let _dest = ctx.get_vreg(mf, inst.id, class);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            });
        }
    }
}

/// Select machine instructions for a terminator.
fn select_terminator(
    mf: &mut MachineFunction,
    ctx: &mut ISelCtx,
    mb: MBlockId,
    term: &Terminator,
    src_block: &BasicBlock,
    func: &Function,
) {
    let _ = src_block; // used implicitly via `term`'s args; kept for clarity
    match term {
        Terminator::Return(None) => {
            emit_epilogue(mf, mb);
        }
        Terminator::Return(Some(val)) => {
            if matches!(func.value_type(*val), Some(IrType::Int(IntWidth::I128))) {
                let src_slot = ctx.lookup_wide_slot(*val);
                emit_load_phys_i128_pair(
                    mf,
                    mb,
                    MachineOperand::PhysReg(PhysReg::FP),
                    src_slot as i64,
                    PhysReg::Gp(0),
                    PhysReg::Gp(1),
                );
                emit_epilogue(mf, mb);
                return;
            }
            // Move result to X0 (integer) or D0 (float).
            let src = ctx.lookup_vreg(*val);
            let class = mf.vregs.iter().find(|v| v.id == src).map(|v| v.class);
            let (reg, opcode) = match class {
                Some(RegClass::Fp64) => (PhysReg::Fp(0), ArmOpcode::FmovReg),
                Some(RegClass::Fp32) => (PhysReg::Fp32(0), ArmOpcode::FmovReg),
                Some(RegClass::Gp32) => (PhysReg::Gp32(0), ArmOpcode::MovReg),
                _ => (PhysReg::Gp(0), ArmOpcode::MovReg),
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::PhysReg(reg), MachineOperand::VReg(src)],
                def: None,
            });
            emit_epilogue(mf, mb);
        }
        Terminator::Branch(dest, args) => {
            // Emit parallel copy from each branch arg into the
            // target block's corresponding param vreg BEFORE the
            // actual branch instruction. Without this, block
            // parameters introduced by mem2reg or the lowerer
            // would never receive their incoming values at edge
            // points, producing infinite loops or stale data.
            emit_branch_arg_copies(mf, ctx, mb, *dest, args);
            let target = ctx.lookup_block(*dest);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::B,
                operands: vec![MachineOperand::BlockRef(target)],
                def: None,
            });
        }
        Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        } => {
            let cond_vreg = ctx.lookup_vreg(*cond);
            let true_mb = ctx.lookup_block(*true_dest);
            let false_mb = ctx.lookup_block(*false_dest);

            // For a conditional branch, the parallel copies for
            // the two arms must happen only on the taken edge. We
            // emit the copies inside per-arm trampoline sequences:
            //
            //   CMP cond, #0
            //   B.EQ false_copies_then_jump   (conditional jump to
            //                                  false-side copies)
            //   <true copies>
            //   B true_dest
            //  false_copies_then_jump:
            //   <false copies>
            //   B false_dest
            //
            // To keep the machine CFG simple we instead emit the
            // false-side copies + jump as a new machine block.
            // But that's invasive. For the common case where
            // neither arm has copies, fall back to the original
            // shape. When either arm has copies, materialize a
            // shim block for that arm.
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::CmpImm,
                operands: vec![MachineOperand::VReg(cond_vreg), MachineOperand::Imm(0)],
                def: None,
            });

            // True arm: if there are branch args to copy, create
            // a shim block that does the copies then jumps to the
            // true destination. Otherwise, branch directly.
            let true_target = if true_args.is_empty() {
                true_mb
            } else {
                // Prefix with the function name so labels stay
                // unique across functions in the same .s file. Two
                // functions could otherwise both emit `L3_true_shim`.
                let label = format!("L{}_{}_true_shim", mf.name, mb.0);
                let shim = mf.new_block(&label);
                emit_branch_arg_copies(mf, ctx, shim, *true_dest, true_args);
                mf.block_mut(shim).insts.push(MachineInst {
                    opcode: ArmOpcode::B,
                    operands: vec![MachineOperand::BlockRef(true_mb)],
                    def: None,
                });
                shim
            };

            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::BCond,
                operands: vec![
                    MachineOperand::Cond(ArmCond::Ne),
                    MachineOperand::BlockRef(true_target),
                ],
                def: None,
            });

            // False arm: same treatment.
            let false_target = if false_args.is_empty() {
                false_mb
            } else {
                let label = format!("L{}_{}_false_shim", mf.name, mb.0);
                let shim = mf.new_block(&label);
                emit_branch_arg_copies(mf, ctx, shim, *false_dest, false_args);
                mf.block_mut(shim).insts.push(MachineInst {
                    opcode: ArmOpcode::B,
                    operands: vec![MachineOperand::BlockRef(false_mb)],
                    def: None,
                });
                shim
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::B,
                operands: vec![MachineOperand::BlockRef(false_target)],
                def: None,
            });
        }
        Terminator::Switch {
            selector,
            cases,
            default,
        } => {
            let sel_vreg = ctx.lookup_vreg(*selector);
            let default_mb = ctx.lookup_block(*default);

            for (val, dest) in cases {
                let dest_mb = ctx.lookup_block(*dest);
                // CMP selector, #val; B.EQ case_block
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::CmpImm,
                    operands: vec![MachineOperand::VReg(sel_vreg), MachineOperand::Imm(*val)],
                    def: None,
                });
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::BCond,
                    operands: vec![
                        MachineOperand::Cond(ArmCond::Eq),
                        MachineOperand::BlockRef(dest_mb),
                    ],
                    def: None,
                });
            }
            // Default: unconditional branch.
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::B,
                operands: vec![MachineOperand::BlockRef(default_mb)],
                def: None,
            });
        }
        Terminator::Unreachable => {
            // Debug trap — should never execute. brk #1 triggers SIGTRAP.
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Brk,
                operands: vec![MachineOperand::Imm(1)],
                def: None,
            });
        }
    }
}

/// Emit the parallel-copy that materializes branch arguments into
/// the target block's parameter vregs.
///
/// At an SSA block boundary the IR semantics say "all the new values
/// arrive in the target's params simultaneously." On a register
/// machine that means we have to perform multiple `mov` operations
/// such that none of them clobbers a value still needed by another
/// pending move. The classical solution:
///
///  1. Skip identity copies (`dst == src`).
///  2. Repeatedly find a pending copy whose `dst` is **not** also
///     the `src` of some other pending copy. Such a copy is "safe"
///     — emitting it can't trample anything still needed.
///  3. If every remaining copy is part of a cycle (no safe copy
///     exists), break the cycle by moving the head of any pending
///     copy through a freshly-allocated scratch vreg, then continue.
///
/// Cycles arise when block params swap with each other across an
/// edge. The lowerer doesn't currently produce that shape, but
/// mem2reg may once we have more sophisticated reaching-definition
/// flow, so handling it now keeps a future bug out of the IR.
fn emit_branch_arg_copies(
    mf: &mut MachineFunction,
    ctx: &ISelCtx,
    mb: MBlockId,
    target_block: BlockId,
    args: &[ValueId],
) {
    if args.is_empty() {
        return;
    }

    // Look up the target block's param vregs in the same order
    // they appear in the IR (which is also the order they were
    // allocated in Phase 4a, so the i-th arg corresponds to the
    // i-th param).
    let target_params = ctx
        .block_params
        .get(&target_block)
        .expect("isel: branch target not in block_params snapshot");
    if target_params.len() != args.len() {
        // Verifier should reject this — but if it leaks through
        // we want a clear panic, not silent corruption.
        panic!(
            "isel: branch arg count {} ≠ target block param count {}",
            args.len(),
            target_params.len()
        );
    }

    // Build the pending copy lists. Narrow SSA values move through
    // vregs; wide i128 values stay stack-backed and must copy slot to
    // slot through a temporary register pair.
    let mut pending_narrow: Vec<(VRegId, VRegId)> = Vec::with_capacity(args.len());
    let mut pending_wide: Vec<(i32, i32)> = Vec::new();
    for (arg, bp) in args.iter().zip(target_params.iter()) {
        if matches!(bp.ty, IrType::Int(IntWidth::I128)) {
            let dst = ctx.lookup_wide_slot(bp.id);
            let src = ctx.lookup_wide_slot(*arg);
            if dst != src {
                pending_wide.push((dst, src));
            }
            continue;
        }
        let dst = ctx.lookup_vreg(bp.id);
        let src = ctx.lookup_vreg(*arg);
        if dst != src {
            pending_narrow.push((dst, src));
        }
    }

    // Helper to look up a vreg's RegClass via mf.vregs.
    fn class_of(mf: &MachineFunction, v: VRegId) -> RegClass {
        mf.vregs
            .iter()
            .find(|r| r.id == v)
            .map(|r| r.class)
            .expect("isel: vreg not registered")
    }

    // Helper to choose the right move opcode for a vreg's class.
    fn move_opcode_for(class: RegClass) -> ArmOpcode {
        match class {
            RegClass::Fp64 | RegClass::Fp32 => ArmOpcode::FmovReg,
            RegClass::Gp64 | RegClass::Gp32 => ArmOpcode::MovReg,
        }
    }

    let emit_move = |mf: &mut MachineFunction, mb: MBlockId, dst: VRegId, src: VRegId| {
        let class = class_of(mf, dst);
        let opcode = move_opcode_for(class);
        mf.block_mut(mb).insts.push(MachineInst {
            opcode,
            operands: vec![MachineOperand::VReg(dst), MachineOperand::VReg(src)],
            def: Some(dst),
        });
    };

    // Iteratively emit safe narrow moves; break cycles via a scratch
    // vreg of the same class.
    let mut pending = pending_narrow;
    while !pending.is_empty() {
        let safe_idx = (0..pending.len()).find(|&i| {
            let (d, _) = pending[i];
            !pending
                .iter()
                .enumerate()
                .any(|(j, &(_, s))| j != i && s == d)
        });

        if let Some(idx) = safe_idx {
            let (d, s) = pending.remove(idx);
            emit_move(mf, mb, d, s);
        } else {
            let (d, s) = pending[0];
            let class = class_of(mf, s);
            let temp = mf.new_vreg(class);
            emit_move(mf, mb, temp, s);
            pending[0] = (d, temp);
        }
    }

    // Wide i128 block params stay stack-backed, so the same parallel-copy
    // algorithm runs on stack slots instead of vregs.
    let mut pending = pending_wide;
    let mut scratch_slot: Option<i32> = None;
    while !pending.is_empty() {
        let safe_idx = (0..pending.len()).find(|&i| {
            let (d, _) = pending[i];
            !pending
                .iter()
                .enumerate()
                .any(|(j, &(_, s))| j != i && s == d)
        });

        if let Some(idx) = safe_idx {
            let (d, s) = pending.remove(idx);
            emit_copy_wide_slot(mf, mb, s, d);
        } else {
            let (d, s) = pending[0];
            let temp = if let Some(slot) = scratch_slot {
                slot
            } else {
                let slot = mf.alloc_local(16);
                scratch_slot = Some(slot);
                slot
            };
            emit_copy_wide_slot(mf, mb, s, temp);
            pending[0] = (d, temp);
        }
    }
}

fn emit_copy_wide_slot(mf: &mut MachineFunction, mb: MBlockId, src_slot: i32, dst_slot: i32) {
    emit_load_phys_i128_pair(
        mf,
        mb,
        MachineOperand::PhysReg(PhysReg::FP),
        src_slot as i64,
        PhysReg::Gp(16),
        PhysReg::Gp(17),
    );
    emit_store_phys_i128_pair(
        mf,
        mb,
        MachineOperand::PhysReg(PhysReg::FP),
        dst_slot as i64,
        PhysReg::Gp(16),
        PhysReg::Gp(17),
    );
}

// ---- Helpers ----

/// Emit function prologue:
///   stp x29, x30, [sp, #-FRAME_SIZE]!
///   add x29, sp, #FRAME_SIZE - 16
/// FP points at the saved FP/LR pair at the top of the frame.
fn emit_prologue(mf: &mut MachineFunction, mb: MBlockId) {
    // STP x29, x30, [sp, #-FRAME_SIZE]!
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::StpPre,
        operands: vec![
            MachineOperand::PhysReg(PhysReg::FP),
            MachineOperand::PhysReg(PhysReg::LR),
            MachineOperand::PhysReg(PhysReg::Sp),
        ],
        def: None,
    });
    // ADD x29, sp, #FRAME_SIZE - 16
    // (frame_size - 16 computed during emission when final size is known)
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::AddImm,
        operands: vec![
            MachineOperand::PhysReg(PhysReg::FP),
            MachineOperand::PhysReg(PhysReg::Sp),
            MachineOperand::Imm(-1), // sentinel: replaced with frame_size-16 during emit
        ],
        def: None,
    });
}

/// Emit function epilogue:
///   ldp x29, x30, [sp, #FRAME_SIZE-16]
///   add sp, sp, #FRAME_SIZE
///   ret
fn emit_epilogue(mf: &mut MachineFunction, mb: MBlockId) {
    // LDP + ADD emitted as a single LdpPost pseudo-op, expanded during emit.
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::LdpPost,
        operands: vec![
            MachineOperand::PhysReg(PhysReg::FP),
            MachineOperand::PhysReg(PhysReg::LR),
            MachineOperand::PhysReg(PhysReg::Sp),
        ],
        def: None,
    });
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::Ret,
        operands: vec![],
        def: None,
    });
}

fn split_i128_words(value: i128) -> (u64, u64) {
    let bits = value as u128;
    (bits as u64, (bits >> 64) as u64)
}

fn emit_const_u64_phys(mf: &mut MachineFunction, mb: MBlockId, dest: PhysReg, value: u64) {
    if value == 0 {
        mf.block_mut(mb).insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(dest),
                MachineOperand::PhysReg(PhysReg::Xzr),
            ],
            def: None,
        });
        return;
    }

    let mut first = true;
    for i in 0..4 {
        let shift = i * 16;
        let chunk = ((value >> shift) & 0xFFFF) as u16;
        if chunk != 0 || (first && i == 3) {
            let opcode = if first {
                ArmOpcode::Movz
            } else {
                ArmOpcode::Movk
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![
                    MachineOperand::PhysReg(dest),
                    MachineOperand::Imm(chunk as i64),
                    MachineOperand::Shift(shift as u8),
                ],
                def: None,
            });
            first = false;
        }
    }
}

fn emit_const_i128_to_phys_pair(
    mf: &mut MachineFunction,
    mb: MBlockId,
    value: i128,
    lo: PhysReg,
    hi: PhysReg,
) {
    let (low_word, high_word) = split_i128_words(value);
    emit_const_u64_phys(mf, mb, lo, low_word);
    emit_const_u64_phys(mf, mb, hi, high_word);
}

fn emit_store_phys_i128_pair(
    mf: &mut MachineFunction,
    mb: MBlockId,
    base: MachineOperand,
    offset: i64,
    lo: PhysReg,
    hi: PhysReg,
) {
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::StpOffset,
        operands: vec![
            MachineOperand::PhysReg(lo),
            MachineOperand::PhysReg(hi),
            base,
            MachineOperand::Imm(offset),
        ],
        def: None,
    });
}

fn emit_load_phys_u64(
    mf: &mut MachineFunction,
    mb: MBlockId,
    base: MachineOperand,
    offset: i64,
    dest: PhysReg,
) {
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::LdrImm,
        operands: vec![
            MachineOperand::PhysReg(dest),
            base,
            MachineOperand::Imm(offset),
        ],
        def: None,
    });
}

fn emit_load_phys_i128_pair(
    mf: &mut MachineFunction,
    mb: MBlockId,
    base: MachineOperand,
    offset: i64,
    lo: PhysReg,
    hi: PhysReg,
) {
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::LdpOffset,
        operands: vec![
            MachineOperand::PhysReg(lo),
            MachineOperand::PhysReg(hi),
            base,
            MachineOperand::Imm(offset),
        ],
        def: None,
    });
}

fn emit_load_stack_arg_into_vreg(
    mf: &mut MachineFunction,
    mb: MBlockId,
    dest: VRegId,
    class: RegClass,
    ty: &IrType,
    offset: i64,
) {
    let opcode = load_opcode_for(ty, class);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode,
        operands: vec![
            MachineOperand::VReg(dest),
            MachineOperand::PhysReg(PhysReg::FP),
            MachineOperand::Imm(offset),
        ],
        def: Some(dest),
    });
}

fn emit_store_stack_arg_from_vreg(
    mf: &mut MachineFunction,
    mb: MBlockId,
    src: VRegId,
    class: RegClass,
    ty: &IrType,
    offset: i64,
) {
    let opcode = store_opcode_for(Some(ty), class);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode,
        operands: vec![
            MachineOperand::VReg(src),
            MachineOperand::PhysReg(PhysReg::Sp),
            MachineOperand::Imm(offset),
        ],
        def: None,
    });
}

fn emit_i128_add_from_slot(
    mf: &mut MachineFunction,
    mb: MBlockId,
    rhs_base: MachineOperand,
    rhs_offset: i64,
    lo: PhysReg,
    hi: PhysReg,
    scratch: PhysReg,
) {
    emit_load_phys_u64(mf, mb, rhs_base.clone(), rhs_offset, scratch);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::AddsReg,
        operands: vec![
            MachineOperand::PhysReg(lo),
            MachineOperand::PhysReg(lo),
            MachineOperand::PhysReg(scratch),
        ],
        def: None,
    });
    emit_load_phys_u64(mf, mb, rhs_base, rhs_offset + 8, scratch);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::AdcReg,
        operands: vec![
            MachineOperand::PhysReg(hi),
            MachineOperand::PhysReg(hi),
            MachineOperand::PhysReg(scratch),
        ],
        def: None,
    });
}

fn emit_i128_sub_from_slot(
    mf: &mut MachineFunction,
    mb: MBlockId,
    rhs_base: MachineOperand,
    rhs_offset: i64,
    lo: PhysReg,
    hi: PhysReg,
    scratch: PhysReg,
) {
    emit_load_phys_u64(mf, mb, rhs_base.clone(), rhs_offset, scratch);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::SubsReg,
        operands: vec![
            MachineOperand::PhysReg(lo),
            MachineOperand::PhysReg(lo),
            MachineOperand::PhysReg(scratch),
        ],
        def: None,
    });
    emit_load_phys_u64(mf, mb, rhs_base, rhs_offset + 8, scratch);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::SbcReg,
        operands: vec![
            MachineOperand::PhysReg(hi),
            MachineOperand::PhysReg(hi),
            MachineOperand::PhysReg(scratch),
        ],
        def: None,
    });
}

fn emit_i128_neg(mf: &mut MachineFunction, mb: MBlockId, lo: PhysReg, hi: PhysReg) {
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::SubsReg,
        operands: vec![
            MachineOperand::PhysReg(lo),
            MachineOperand::PhysReg(PhysReg::Xzr),
            MachineOperand::PhysReg(lo),
        ],
        def: None,
    });
    mf.block_mut(mb).insts.push(MachineInst {
        opcode: ArmOpcode::SbcReg,
        operands: vec![
            MachineOperand::PhysReg(hi),
            MachineOperand::PhysReg(PhysReg::Xzr),
            MachineOperand::PhysReg(hi),
        ],
        def: None,
    });
}

/// Emit a constant integer using movz/movk sequence.
/// Respects width: 32-bit values mask to 32 bits and only emit shifts 0/16.
fn emit_const_int(
    mf: &mut MachineFunction,
    mb: MBlockId,
    dest: VRegId,
    val: i128,
    width: IntWidth,
) {
    debug_assert!(
        width != IntWidth::I128,
        "backend should reject i128 before isel"
    );
    let is_32 = matches!(width, IntWidth::I8 | IntWidth::I16 | IntWidth::I32);
    // Mask to the appropriate width to prevent sign-extension artifacts.
    let uval = if is_32 {
        (val as u32) as u64
    } else {
        val as u64
    };
    let max_shift = if is_32 { 2 } else { 4 }; // 2 chunks for 32-bit, 4 for 64-bit

    if uval == 0 {
        let zr = if is_32 { PhysReg::Wzr } else { PhysReg::Xzr };
        mf.block_mut(mb).insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![MachineOperand::VReg(dest), MachineOperand::PhysReg(zr)],
            def: Some(dest),
        });
        return;
    }

    // MOVZ for the first non-zero 16-bit chunk, MOVK for the rest.
    let mut first = true;
    for i in 0..max_shift {
        let shift = i * 16;
        let chunk = ((uval >> shift) & 0xFFFF) as u16;
        if chunk != 0 || (first && i == max_shift - 1) {
            let opcode = if first {
                ArmOpcode::Movz
            } else {
                ArmOpcode::Movk
            };
            mf.block_mut(mb).insts.push(MachineInst {
                opcode,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::Imm(chunk as i64),
                    MachineOperand::Shift(shift as u8),
                ],
                def: Some(dest),
            });
            first = false;
        }
    }

    if first {
        let zr = if is_32 { PhysReg::Wzr } else { PhysReg::Xzr };
        mf.block_mut(mb).insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![MachineOperand::VReg(dest), MachineOperand::PhysReg(zr)],
            def: Some(dest),
        });
    }
}

/// Emit a register-register binary op.
fn emit_binop(
    mf: &mut MachineFunction,
    ctx: &mut ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    opcode: ArmOpcode,
    a: ValueId,
    b: ValueId,
) {
    let class = type_to_reg_class(&inst.ty);
    let dest = ctx.get_vreg(mf, inst.id, class);
    let va = ctx.lookup_vreg(a);
    let vb = ctx.lookup_vreg(b);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode,
        operands: vec![
            MachineOperand::VReg(dest),
            MachineOperand::VReg(va),
            MachineOperand::VReg(vb),
        ],
        def: Some(dest),
    });
}

/// Emit a float binary op, selecting single or double precision.
#[allow(clippy::too_many_arguments)]
fn emit_float_binop(
    mf: &mut MachineFunction,
    ctx: &mut ISelCtx,
    mb: MBlockId,
    inst: &Inst,
    ty: &IrType,
    a: ValueId,
    b: ValueId,
    op_s: ArmOpcode,
    op_d: ArmOpcode,
) {
    let (class, opcode) = match ty {
        IrType::Float(FloatWidth::F32) => (RegClass::Fp32, op_s),
        _ => (RegClass::Fp64, op_d),
    };
    let dest = ctx.get_vreg(mf, inst.id, class);
    let va = ctx.lookup_vreg(a);
    let vb = ctx.lookup_vreg(b);
    mf.block_mut(mb).insts.push(MachineInst {
        opcode,
        operands: vec![
            MachineOperand::VReg(dest),
            MachineOperand::VReg(va),
            MachineOperand::VReg(vb),
        ],
        def: Some(dest),
    });
}

/// Map IR type to register class.
/// Pick the load opcode for a value of the given IR type and reg class.
/// Narrow integer types use the sign-extending byte/half loads; floats
/// route to the FP-imm load; everything else falls through to `LdrImm`
/// or `LdrFpImm` per reg class. The reg-class fallback matters when
/// `ty` is a generic pointer or aggregate (e.g., a stack-arg copy that
/// only knows the destination's register kind).
fn load_opcode_for(ty: &IrType, class: RegClass) -> ArmOpcode {
    match ty {
        IrType::Int(IntWidth::I8) | IrType::Bool => ArmOpcode::LdrsbImm,
        IrType::Int(IntWidth::I16) => ArmOpcode::LdrshImm,
        IrType::Float(_) => ArmOpcode::LdrFpImm,
        _ => match class {
            RegClass::Fp64 | RegClass::Fp32 => ArmOpcode::LdrFpImm,
            RegClass::Gp32 | RegClass::Gp64 => ArmOpcode::LdrImm,
        },
    }
}

/// Mirror of `load_opcode_for` for stores. Audit CRITICAL-2: the
/// `ty` here must be the *value's* declared IR type, not the pointer
/// or pointee — byte-level GEPs reuse `ptr<i8>` as a generic offset
/// cursor, so dispatching by pointee width would silently truncate
/// non-byte stores. Pass `None` for `ty` when only the reg class is
/// available; in that case the helper falls through to the class-only
/// branch.
fn store_opcode_for(ty: Option<&IrType>, class: RegClass) -> ArmOpcode {
    match ty {
        Some(IrType::Int(IntWidth::I8)) | Some(IrType::Bool) => ArmOpcode::StrbImm,
        Some(IrType::Int(IntWidth::I16)) => ArmOpcode::StrhImm,
        Some(IrType::Float(_)) => ArmOpcode::StrFpImm,
        _ => match class {
            RegClass::Fp64 | RegClass::Fp32 => ArmOpcode::StrFpImm,
            RegClass::Gp32 | RegClass::Gp64 => ArmOpcode::StrImm,
        },
    }
}

/// Resolve an IR address value to the (base, offset) operand pair
/// expected by `LdrImm`/`StrImm`-family instructions. Alloca addresses
/// fold to `(FP, FrameSlot(offset))` so the assembler can pick the
/// final stack-relative form; everything else becomes
/// `(VReg(addr_vreg), Imm(0))`. Used by both narrow-width Load/Store
/// arms in `select_inst`. The wide-i128 paths build their own operand
/// pairs directly because they target the `emit_*_phys_i128_pair`
/// helpers, which take `i64` offsets and only need a base operand.
fn narrow_load_store_addr(
    ctx: &ISelCtx,
    addr: ValueId,
) -> (MachineOperand, MachineOperand) {
    if let Some(&offset) = ctx.alloca_offsets.get(&addr) {
        (
            MachineOperand::PhysReg(PhysReg::FP),
            MachineOperand::FrameSlot(offset),
        )
    } else {
        let base = ctx.lookup_vreg(addr);
        (MachineOperand::VReg(base), MachineOperand::Imm(0))
    }
}

/// Operation tag for `emit_i128_binop_via_slots`. Add and Sub share a
/// load-binop-store skeleton that differs only in which intermediate
/// helper does the arithmetic.
#[derive(Clone, Copy)]
enum I128BinOp {
    Add,
    Sub,
}

/// Lower an i128 IAdd/ISub: load `lhs_id`'s slot into x16/x17, run the
/// matching `emit_i128_<op>_from_slot` against `rhs_id`, then store
/// the result to `dest_id`'s slot. Replaces three near-identical 30-LOC
/// blocks in the i128 dispatch (IAdd / ISub).
fn emit_i128_binop_via_slots(
    mf: &mut MachineFunction,
    ctx: &ISelCtx,
    mb: MBlockId,
    op: I128BinOp,
    dest_id: ValueId,
    lhs_id: ValueId,
    rhs_id: ValueId,
) {
    let dest_slot = ctx.lookup_wide_slot(dest_id);
    let lhs_slot = ctx.lookup_wide_slot(lhs_id);
    let rhs_slot = ctx.lookup_wide_slot(rhs_id);
    let fp = || MachineOperand::PhysReg(PhysReg::FP);
    emit_load_phys_i128_pair(mf, mb, fp(), lhs_slot as i64, PhysReg::Gp(16), PhysReg::Gp(17));
    match op {
        I128BinOp::Add => emit_i128_add_from_slot(
            mf,
            mb,
            fp(),
            rhs_slot as i64,
            PhysReg::Gp(16),
            PhysReg::Gp(17),
            PhysReg::Gp(8),
        ),
        I128BinOp::Sub => emit_i128_sub_from_slot(
            mf,
            mb,
            fp(),
            rhs_slot as i64,
            PhysReg::Gp(16),
            PhysReg::Gp(17),
            PhysReg::Gp(8),
        ),
    }
    emit_store_phys_i128_pair(mf, mb, fp(), dest_slot as i64, PhysReg::Gp(16), PhysReg::Gp(17));
}

fn type_to_reg_class(ty: &IrType) -> RegClass {
    match ty {
        IrType::Float(FloatWidth::F32) => RegClass::Fp32,
        IrType::Float(FloatWidth::F64) => RegClass::Fp64,
        IrType::Int(IntWidth::I8)
        | IrType::Int(IntWidth::I16)
        | IrType::Int(IntWidth::I32)
        | IrType::Bool => RegClass::Gp32,
        _ => RegClass::Gp64,
    }
}

fn needs_wide_icmp_operand(ty: Option<&IrType>, other_ty: Option<&IrType>) -> bool {
    matches!(
        (ty, other_ty),
        (
            Some(IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_)),
            Some(_)
        ) | (
            Some(_),
            Some(IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_))
        )
    )
}

fn zero_extend_cmp_type(ty: Option<&IrType>) -> bool {
    matches!(ty, Some(IrType::Bool))
}

fn icmp_operand_vreg(
    mf: &mut MachineFunction,
    ctx: &mut ISelCtx,
    mb: MBlockId,
    func: &Function,
    value: ValueId,
    other: ValueId,
) -> VRegId {
    let value_ty = func.value_type(value);
    let other_ty = func.value_type(other);
    let src = ctx.lookup_vreg(value);

    if !needs_wide_icmp_operand(value_ty.as_ref(), other_ty.as_ref()) {
        return src;
    }

    if matches!(
        value_ty,
        Some(IrType::Int(IntWidth::I64) | IrType::Ptr(_) | IrType::FuncPtr(_))
    ) {
        return src;
    }

    let dest = mf.new_vreg(RegClass::Gp64);
    let opcode = if zero_extend_cmp_type(value_ty.as_ref()) {
        ArmOpcode::MovReg
    } else {
        ArmOpcode::Sxtw
    };
    mf.block_mut(mb).insts.push(MachineInst {
        opcode,
        operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
        def: Some(dest),
    });
    dest
}

fn machine_vreg_class(mf: &MachineFunction, vreg: VRegId) -> RegClass {
    mf.vregs
        .iter()
        .find(|r| r.id == vreg)
        .map(|r| r.class)
        .expect("isel: vreg not registered")
}

fn coerce_select_operand_vreg(
    mf: &mut MachineFunction,
    ctx: &mut ISelCtx,
    mb: MBlockId,
    func: &Function,
    value: ValueId,
    target_ty: &IrType,
) -> VRegId {
    let src = ctx.lookup_vreg(value);
    let src_class = machine_vreg_class(mf, src);
    let target_class = type_to_reg_class(target_ty);
    if src_class == target_class {
        return src;
    }

    let dest = mf.new_vreg(target_class);
    let src_ty = func.value_type(value);
    let opcode = match (src_class, target_class) {
        (RegClass::Gp32, RegClass::Gp64) => {
            if matches!(target_ty, IrType::Ptr(_) | IrType::FuncPtr(_))
                || zero_extend_cmp_type(src_ty.as_ref())
            {
                ArmOpcode::MovReg
            } else {
                match src_ty.as_ref() {
                    Some(IrType::Int(IntWidth::I8)) => ArmOpcode::Sxtb,
                    Some(IrType::Int(IntWidth::I16)) => ArmOpcode::Sxth,
                    Some(IrType::Int(IntWidth::I32)) | Some(IrType::Bool) => ArmOpcode::Sxtw,
                    _ => ArmOpcode::MovReg,
                }
            }
        }
        (RegClass::Gp64, RegClass::Gp32) => ArmOpcode::MovReg,
        (RegClass::Fp32, RegClass::Fp64) => ArmOpcode::FcvtDS,
        (RegClass::Fp64, RegClass::Fp32) => ArmOpcode::FcvtSD,
        _ => ArmOpcode::MovReg,
    };

    mf.block_mut(mb).insts.push(MachineInst {
        opcode,
        operands: vec![MachineOperand::VReg(dest), MachineOperand::VReg(src)],
        def: Some(dest),
    });
    dest
}

fn int_width_class(w: &IntWidth) -> RegClass {
    match w {
        IntWidth::I64 => RegClass::Gp64,
        _ => RegClass::Gp32,
    }
}

fn float_width_class(w: &FloatWidth) -> RegClass {
    match w {
        FloatWidth::F32 => RegClass::Fp32,
        FloatWidth::F64 => RegClass::Fp64,
    }
}

/// Map IR comparison op to ARM64 condition code (for integer CMP).
/// Pre-scan a function to find ICmp/FCmp → Select fusion candidates.
///
/// An ICmp/FCmp is a fusion candidate when:
/// 1. Its result is used exactly once in the entire function.
/// 2. That single use is a `Select` instruction in the same block.
/// 3. No intervening instruction between the ICmp and the Select in
///    that block clobbers NZCV flags (another ICmp/FCmp or a Call).
///
/// For candidates, we suppress CSET during ICmp lowering and store
/// the ARM condition in `ctx.fused_arm_cond` so the Select can pick
/// it up and emit `CSEL dest, tv, fv, <cond>` directly.
fn compute_csel_fusion(func: &Function, ctx: &mut ISelCtx) {
    // Build global use counts.
    let mut use_count: HashMap<ValueId, u32> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for vid in crate::ir::walk::inst_uses(&inst.kind) {
                *use_count.entry(vid).or_insert(0) += 1;
            }
        }
        if let Some(term) = &block.terminator {
            for vid in crate::ir::walk::terminator_uses(term) {
                *use_count.entry(vid).or_insert(0) += 1;
            }
        }
    }

    // Build a map of ValueId → the block that defines it (instruction defs only).
    let mut def_block: HashMap<ValueId, BlockId> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            def_block.insert(inst.id, block.id);
        }
    }

    // Per-block scan: walk instructions in order, tracking the most
    // recent ICmp/FCmp that hasn't been consumed by a Select yet.
    // Any flag-clobbering instruction (another ICmp/FCmp, a call)
    // resets the pending set.
    for block in &func.blocks {
        // The most recently emitted CMP that hasn't been consumed.
        // We use a Vec so that `pending = {last_icmp}` is O(1) to update.
        let mut pending: Option<ValueId> = None;

        for inst in &block.insts {
            match &inst.kind {
                InstKind::ICmp(op, _, _) => {
                    if crate::ir::walk::inst_uses(&inst.kind)
                        .into_iter()
                        .filter_map(|vid| func.value_type(vid))
                        .any(|ty| matches!(ty, IrType::Int(IntWidth::I128)))
                    {
                        pending = None;
                        ctx.fused_arm_cond.remove(&inst.id);
                        continue;
                    }
                    // New CMP overwrites NZCV — previous pending is no longer valid.
                    pending = Some(inst.id);
                    // Temporarily store the arm cond so we can retrieve it when
                    // we confirm the Select is the sole user.
                    ctx.fused_arm_cond.insert(inst.id, cmp_to_arm_cond(*op));
                }
                InstKind::FCmp(op, _, _) => {
                    pending = Some(inst.id);
                    ctx.fused_arm_cond.insert(inst.id, fcmp_to_arm_cond(*op));
                }
                InstKind::Select(cond, _, _) => {
                    if let Some(p) = pending {
                        if p == *cond
                            && use_count.get(cond) == Some(&1)
                            && def_block.get(cond) == Some(&block.id)
                        {
                            // Confirmed: fuse this ICmp into the Select.
                            ctx.select_fused.insert(*cond);
                            pending = None;
                        } else {
                            // The Select isel for an unfused cond emits
                            // its own `cmp cond_reg, #0` to set NZCV,
                            // which clobbers any pending fused ICmp's
                            // flags.  Drop the pending so a later Select
                            // doesn't try to read stale flags.
                            pending = None;
                        }
                    }
                }
                // Calls may clobber NZCV (per AAPCS64, flags are not preserved).
                InstKind::Call(_, _) | InstKind::RuntimeCall(_, _) => {
                    pending = None;
                }
                _ => {}
            }
        }

        // Clean up fused_arm_cond for ICmps that turned out NOT to be fused
        // (e.g., they had use_count > 1, or were never consumed by a Select).
        // Leave only the fused ones.
        //
        // We delay cleanup to after all blocks are scanned because the same
        // ValueId can't appear in multiple blocks (SSA), so there's no cross-
        // block confusion.
    }

    // Remove arm_cond entries for non-fused ICmps.
    ctx.fused_arm_cond
        .retain(|vid, _| ctx.select_fused.contains(vid));
}

fn cmp_to_arm_cond(op: CmpOp) -> ArmCond {
    match op {
        CmpOp::Eq => ArmCond::Eq,
        CmpOp::Ne => ArmCond::Ne,
        CmpOp::Lt => ArmCond::Lt,
        CmpOp::Le => ArmCond::Le,
        CmpOp::Gt => ArmCond::Gt,
        CmpOp::Ge => ArmCond::Ge,
    }
}

fn i128_ordered_conds(op: CmpOp) -> (ArmCond, ArmCond) {
    match op {
        CmpOp::Lt => (ArmCond::Lt, ArmCond::Lo),
        CmpOp::Le => (ArmCond::Lt, ArmCond::Ls),
        CmpOp::Gt => (ArmCond::Gt, ArmCond::Hi),
        CmpOp::Ge => (ArmCond::Gt, ArmCond::Hs),
        _ => panic!("ordered i128 compare requires lt/le/gt/ge, got {:?}", op),
    }
}

/// Map IR comparison op to ARM64 condition code (for float FCMP).
fn fcmp_to_arm_cond(op: CmpOp) -> ArmCond {
    match op {
        CmpOp::Eq => ArmCond::Eq,
        CmpOp::Ne => ArmCond::Ne,
        CmpOp::Lt => ArmCond::Mi, // minus flag for less-than
        CmpOp::Le => ArmCond::Ls, // unsigned LE maps to float LE
        CmpOp::Gt => ArmCond::Gt,
        CmpOp::Ge => ArmCond::Ge,
    }
}

/// Compute allocation size for an IR type.
fn alloca_size(ty: &IrType) -> u32 {
    match ty {
        IrType::Void => 0,
        IrType::Bool => 4, // use 4 bytes for alignment
        IrType::Int(w) => w.bytes(),
        IrType::Float(w) => w.bytes(),
        IrType::Ptr(_) => 8,
        IrType::Array(elem, count) => {
            // Stack storage uses ABI-sized elements. Fortran LOGICAL arrays are
            // stored as default-kind 4-byte elements, even though Bool SSA
            // values themselves remain byte-sized.
            let elem_size = match elem.as_ref() {
                IrType::Bool => 4,
                IrType::Struct(_) => alloca_size(elem),
                _ => elem.size_bytes() as u32,
            };
            elem_size * (*count as u32)
        }
        IrType::FuncPtr(_) => 8,
        IrType::Struct(_) => 8, // placeholder
        IrType::Vector { .. } => 16, // 128-bit NEON
    }
}

/// Get the symbol name for a runtime function.
/// Get the C-level symbol name for a runtime function.
/// The emitter adds the Mach-O `_` prefix when emitting assembly.
fn runtime_func_symbol(rf: &RuntimeFunc, args: &[(ValueId, AbiArgLoc, IrType)]) -> String {
    match rf {
        RuntimeFunc::PrintInt => {
            if args
                .first()
                .is_some_and(|(_, _, ty)| matches!(ty, IrType::Int(IntWidth::I128)))
            {
                "afs_print_int128".into()
            } else if args
                .first()
                .is_some_and(|(_, _, ty)| matches!(ty, IrType::Int(IntWidth::I64)))
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

    fn select_simple(build: impl FnOnce(&mut FuncBuilder)) -> MachineFunction {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            build(&mut b);
        }
        select_function(&func)
    }

    #[test]
    fn select_const_int() {
        let mf = select_simple(|b| {
            b.const_i32(42);
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        // Should have: prologue (STP, MOV), MOVZ #42, epilogue (LDP, RET).
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::Movz));
    }

    #[test]
    fn select_iadd() {
        let mf = select_simple(|b| {
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        });
        assert!(mf.blocks[0]
            .insts
            .iter()
            .any(|i| i.opcode == ArmOpcode::AddReg));
    }

    #[test]
    fn select_icmp() {
        // ICmp whose result is NOT fed into a Select → CSET must appear.
        let mf = select_simple(|b| {
            let x = b.const_i32(5);
            let y = b.const_i32(10);
            let _c = b.icmp(CmpOp::Lt, x, y);
            b.ret_void();
        });
        assert!(mf.blocks[0]
            .insts
            .iter()
            .any(|i| i.opcode == ArmOpcode::CmpReg));
        assert!(mf.blocks[0]
            .insts
            .iter()
            .any(|i| i.opcode == ArmOpcode::Cset));
    }

    #[test]
    fn select_i128_icmp_eq_combines_limb_results() {
        let mf = select_simple(|b| {
            let x = b.const_i128(1);
            let y = b.const_i128(1);
            let _c = b.icmp(CmpOp::Eq, x, y);
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        assert!(
            insts
                .iter()
                .filter(|i| i.opcode == ArmOpcode::CmpReg)
                .count()
                >= 2
        );
        assert!(insts.iter().filter(|i| i.opcode == ArmOpcode::Cset).count() >= 2);
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::AndReg));
    }

    #[test]
    fn select_i128_icmp_lt_uses_high_signed_and_low_unsigned_conds() {
        let mf = select_simple(|b| {
            let x = b.const_i128(1);
            let y = b.const_i128(2);
            let _c = b.icmp(CmpOp::Lt, x, y);
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        assert!(
            insts
                .iter()
                .filter(|i| i.opcode == ArmOpcode::CmpReg)
                .count()
                >= 2
        );
        assert!(insts.iter().filter(|i| i.opcode == ArmOpcode::Cset).count() >= 3);
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::AndReg));
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::OrrReg));
    }

    #[test]
    fn select_i128_uses_pair_csel_ops() {
        let mf = select_simple(|b| {
            let cond = b.const_bool(true);
            let x = b.const_i128(1);
            let y = b.const_i128(2);
            let _s = b.select(cond, x, y);
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::CmpImm));
        assert_eq!(
            insts
                .iter()
                .filter(|i| i.opcode == ArmOpcode::CselReg)
                .count(),
            2,
            "wide i128 selects should lower with one CSEL per limb"
        );
    }

    #[test]
    fn select_coerces_mixed_gp_widths_before_csel() {
        let mf = select_simple(|b| {
            let cond = b.const_bool(true);
            let wide = b.const_i64(7);
            let narrow = b.const_i32(-1);
            let _s = b.select(cond, wide, narrow);
            b.ret_void();
        });
        let csel = mf.blocks[0]
            .insts
            .iter()
            .find(|i| i.opcode == ArmOpcode::CselReg)
            .expect("expected CSEL for mixed-width select");
        for operand in csel.operands.iter().take(3) {
            let MachineOperand::VReg(vreg) = operand else {
                continue;
            };
            assert_eq!(
                machine_vreg_class(&mf, *vreg),
                RegClass::Gp64,
                "mixed-width select operands should be coerced to the result width before CSEL"
            );
        }
    }

    #[test]
    fn csel_fusion_eliminates_cset_and_extra_cmp() {
        // ICmp used solely by a Select → CSET and CMP cond, #0 must NOT appear.
        // Only CmpReg + CselReg should be present.
        let mf = select_simple(|b| {
            let x = b.const_i32(5);
            let y = b.const_i32(10);
            let c = b.icmp(CmpOp::Le, x, y); // use_count[c] = 1, only in Select
            let _s = b.select(c, x, y);
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        // Must have a CMP to set flags.
        assert!(
            insts.iter().any(|i| i.opcode == ArmOpcode::CmpReg),
            "expected CmpReg for ICmp"
        );
        // Must have CSEL to select the value.
        assert!(
            insts.iter().any(|i| i.opcode == ArmOpcode::CselReg),
            "expected CselReg for Select"
        );
        // Must NOT have CSET (ICmp boolean materialization is suppressed).
        assert!(
            !insts.iter().any(|i| i.opcode == ArmOpcode::Cset),
            "CSET should be suppressed when ICmp feeds only a Select"
        );
        // Must NOT have a second CmpImm (CMP cond, #0 is suppressed).
        assert!(
            !insts.iter().any(|i| i.opcode == ArmOpcode::CmpImm),
            "CMP cond,#0 should be suppressed when CSEL uses flags directly"
        );
    }

    #[test]
    fn csel_no_fusion_when_icmp_has_multiple_uses() {
        // ICmp used by both a Select and another instruction → CSET is kept.
        let mf = select_simple(|b| {
            let x = b.const_i32(5);
            let y = b.const_i32(10);
            let c = b.icmp(CmpOp::Le, x, y); // use_count[c] = 2
            let _s = b.select(c, x, y);
            // Also use `c` in a logical NOT to force a second use.
            let _n = b.not(c);
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        // CSET must still be emitted because `c` has multiple uses.
        assert!(
            insts.iter().any(|i| i.opcode == ArmOpcode::Cset),
            "CSET should remain when ICmp has multiple uses"
        );
    }

    #[test]
    fn select_fadd() {
        let mf = select_simple(|b| {
            let x = b.const_f64(1.0);
            let y = b.const_f64(2.0);
            let _z = b.fadd(x, y);
            b.ret_void();
        });
        assert!(mf.blocks[0]
            .insts
            .iter()
            .any(|i| i.opcode == ArmOpcode::FaddD));
    }

    #[test]
    fn select_alloca_and_store() {
        let mf = select_simple(|b| {
            let addr = b.alloca(IrType::Int(IntWidth::I32));
            let val = b.const_i32(42);
            b.store(val, addr);
            b.ret_void();
        });
        // Should have SubImm (address materialization from FP) and StrImm.
        assert!(mf.blocks[0]
            .insts
            .iter()
            .any(|i| i.opcode == ArmOpcode::SubImm));
        assert!(mf.blocks[0]
            .insts
            .iter()
            .any(|i| i.opcode == ArmOpcode::StrImm));
    }

    #[test]
    fn select_branch() {
        let mf = select_simple(|b| {
            let cond = b.const_bool(true);
            let bb_t = b.create_block("then");
            let bb_f = b.create_block("else");
            b.cond_branch(cond, bb_t, vec![], bb_f, vec![]);

            b.set_block(bb_t);
            b.ret_void();
            b.set_block(bb_f);
            b.ret_void();
        });
        // Entry block should have CmpImm + BCond + B.
        assert!(mf.blocks[0]
            .insts
            .iter()
            .any(|i| i.opcode == ArmOpcode::BCond));
    }

    #[test]
    fn select_call() {
        let mf = select_simple(|b| {
            b.runtime_call(crate::ir::inst::RuntimeFunc::PrintInt, vec![], IrType::Void);
            b.ret_void();
        });
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::Bl));
    }

    #[test]
    fn select_call_arg_from_later_block_alloca_has_preallocated_vreg() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let use_block = b.create_block("use");
            let def_block = b.create_block("def");

            b.branch(def_block, vec![]);

            b.set_block(use_block);
            let dummy = b.const_i64(7);
            b.call(
                FuncRef::External("_callee".into()),
                vec![dummy],
                IrType::Void,
            );
            b.ret_void();

            b.set_block(def_block);
            let slot = b.alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            b.call(FuncRef::External("_callee".into()), vec![slot], IrType::Void);
            b.branch(use_block, vec![]);
        }

        let mf = select_function(&func);
        assert!(
            mf.blocks.iter().any(|block| {
                block.insts.iter().any(|inst| {
                    inst.opcode == ArmOpcode::SubImm
                        && matches!(inst.operands.first(), Some(MachineOperand::VReg(_)))
                })
            }),
            "alloca address should materialize into a preallocated vreg",
        );
        assert!(
            mf.blocks
                .iter()
                .flat_map(|block| block.insts.iter())
                .filter(|inst| inst.opcode == ArmOpcode::Bl)
                .count()
                >= 2,
            "both calls should lower successfully without an unmapped alloca arg vreg",
        );
    }

    #[test]
    fn select_i128_runtime_print_uses_wide_symbol_and_pair_regs() {
        let mf = select_simple(|b| {
            let wide = b.const_i128(170141183460469231731687303715884105727i128);
            b.runtime_call(
                crate::ir::inst::RuntimeFunc::PrintInt,
                vec![wide],
                IrType::Void,
            );
            b.ret_void();
        });
        let asm = crate::codegen::emit::emit_function(&mf);
        assert!(
            asm.contains("bl _afs_print_int128"),
            "runtime i128 print should call the wide symbol:\n{}",
            asm
        );
        assert!(
            asm.contains("ldp x0, x1"),
            "runtime i128 print should marshal the value through the pair-register ABI:\n{}",
            asm
        );
    }

    #[test]
    fn prologue_and_epilogue() {
        let mf = select_simple(|b| {
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        assert_eq!(
            insts[0].opcode,
            ArmOpcode::StpPre,
            "first inst should be STP (prologue)"
        );
        assert_eq!(
            insts[1].opcode,
            ArmOpcode::AddImm,
            "second inst should be ADD FP, SP, #offset"
        );
        assert!(
            insts.iter().any(|i| i.opcode == ArmOpcode::Ret),
            "should have RET"
        );
    }

    #[test]
    fn const_zero_uses_zr() {
        let mf = select_simple(|b| {
            b.const_i32(0);
            b.ret_void();
        });
        // const_i32(0) should use MOV dest, WZR (32-bit zero register).
        let insts = &mf.blocks[0].insts;
        let has_mov_zr = insts.iter().any(|i| {
            i.opcode == ArmOpcode::MovReg
                && i.operands.iter().any(|o| {
                    matches!(
                        o,
                        MachineOperand::PhysReg(PhysReg::Xzr)
                            | MachineOperand::PhysReg(PhysReg::Wzr)
                    )
                })
        });
        assert!(has_mov_zr, "const 0 should use MOV from XZR or WZR");
    }

    // ---- Parallel-copy / branch arg copy tests ----
    //
    // The branch arg copy resolver in `emit_branch_arg_copies` handles
    // cross-edge moves into block params. When the source/destination
    // graph contains a cycle, the resolver routes one copy through a
    // scratch vreg. These tests construct minimal IR functions that
    // exercise each topology, run isel, and inspect the resulting move
    // count in the source machine block.

    /// Helper: count vreg→vreg moves of the given opcode in a block,
    /// excluding moves that target a physical register (those are
    /// epilogue/return marshaling, not parallel copies).
    fn count_vreg_moves(block: &MachineBlock, opcode: ArmOpcode) -> usize {
        block
            .insts
            .iter()
            .filter(|i| i.opcode == opcode)
            .filter(|i| {
                // True parallel copies are VReg → VReg.
                matches!(i.operands.first(), Some(MachineOperand::VReg(_)))
                    && matches!(i.operands.get(1), Some(MachineOperand::VReg(_)))
            })
            .count()
    }

    fn find_block<'a>(mf: &'a MachineFunction, contains: &str) -> &'a MachineBlock {
        mf.blocks
            .iter()
            .find(|b| b.label.contains(contains))
            .unwrap_or_else(|| {
                panic!(
                    "no machine block containing '{}' (have: {:?})",
                    contains,
                    mf.blocks.iter().map(|b| &b.label).collect::<Vec<_>>(),
                )
            })
    }

    #[test]
    fn branch_arg_2_cycle_routes_through_scratch() {
        // body branches to header swapping the two int params:
        //   br header(pb, pa)
        // pending = [(pa,pb), (pb,pa)] — pure 2-cycle, requires:
        //   tmp = pb;  pb = pa;  pa = tmp     (3 moves)
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let header = b.create_block("header");
            let pa = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let pb = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let body = b.create_block("body");
            let exit = b.create_block("exit");

            let v0 = b.const_i32(1);
            let v1 = b.const_i32(2);
            b.branch(header, vec![v0, v1]);

            b.set_block(header);
            b.cond_branch(pa, body, vec![], exit, vec![]);

            b.set_block(body);
            b.branch(header, vec![pb, pa]);

            b.set_block(exit);
            b.ret_void();
        }
        let mf = select_function(&func);
        let body_mb = find_block(&mf, "body");
        let moves = count_vreg_moves(body_mb, ArmOpcode::MovReg);
        assert_eq!(
            moves, 3,
            "2-cycle should emit 3 vreg→vreg moves (scratch + 2 swaps), got {}: {:#?}",
            moves, body_mb.insts,
        );
    }

    #[test]
    fn branch_arg_3_cycle_routes_through_scratch() {
        // br header(pb, pc, pa) — rotate three params left.
        // pending = [(pa,pb),(pb,pc),(pc,pa)]
        // Resolution: tmp = pb;  pb = pc;  pc = pa;  pa = tmp   (4 moves)
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let header = b.create_block("header");
            let pa = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let pb = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let pc = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let body = b.create_block("body");
            let exit = b.create_block("exit");

            let v0 = b.const_i32(1);
            let v1 = b.const_i32(2);
            let v2 = b.const_i32(3);
            b.branch(header, vec![v0, v1, v2]);

            b.set_block(header);
            b.cond_branch(pa, body, vec![], exit, vec![]);

            b.set_block(body);
            b.branch(header, vec![pb, pc, pa]);

            b.set_block(exit);
            b.ret_void();
        }
        let mf = select_function(&func);
        let body_mb = find_block(&mf, "body");
        let moves = count_vreg_moves(body_mb, ArmOpcode::MovReg);
        assert_eq!(
            moves, 4,
            "3-cycle should emit 4 vreg→vreg moves (scratch + 3 rotates), got {}: {:#?}",
            moves, body_mb.insts,
        );
    }

    #[test]
    fn branch_arg_cycle_plus_independent_tail() {
        // 2-cycle on (pa,pb) plus an independent (pc <- v_extra) tail.
        // br header(pb, pa, v_extra)
        // The tail (pc, v_extra) is always safe and emits as a single
        // move; the 2-cycle adds 3 moves for a total of 4.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let header = b.create_block("header");
            let pa = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let pb = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let _pc = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let body = b.create_block("body");
            let exit = b.create_block("exit");

            let v0 = b.const_i32(1);
            let v1 = b.const_i32(2);
            let v2 = b.const_i32(3);
            b.branch(header, vec![v0, v1, v2]);

            b.set_block(header);
            b.cond_branch(pa, body, vec![], exit, vec![]);

            b.set_block(body);
            // Body needs a fresh value for pc so it's not part of the
            // cycle and so it can't degenerate into pa/pb.
            let v3 = b.const_i32(99);
            b.branch(header, vec![pb, pa, v3]);

            b.set_block(exit);
            b.ret_void();
        }
        let mf = select_function(&func);
        let body_mb = find_block(&mf, "body");
        let moves = count_vreg_moves(body_mb, ArmOpcode::MovReg);
        assert_eq!(
            moves, 4,
            "cycle+tail should emit 4 vreg→vreg moves (3 for cycle + 1 for tail), got {}: {:#?}",
            moves, body_mb.insts,
        );
    }

    #[test]
    fn branch_arg_mixed_gp_fp_classes() {
        // Two int params and two float params, all swapped pairwise.
        // pending splits into a GP 2-cycle and an FP 2-cycle, each of
        // which independently needs a scratch.
        // Expected: 3 GP MovReg + 3 FP FmovReg = 6 total moves.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let header = b.create_block("header");
            let ia = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let ib = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let fa = b.add_block_param(header, IrType::Float(FloatWidth::F64));
            let fb = b.add_block_param(header, IrType::Float(FloatWidth::F64));
            let body = b.create_block("body");
            let exit = b.create_block("exit");

            let v0 = b.const_i32(1);
            let v1 = b.const_i32(2);
            let f0 = b.const_f64(1.0);
            let f1 = b.const_f64(2.0);
            b.branch(header, vec![v0, v1, f0, f1]);

            b.set_block(header);
            b.cond_branch(ia, body, vec![], exit, vec![]);

            b.set_block(body);
            // Swap both pairs: ints (ib, ia) and floats (fb, fa).
            b.branch(header, vec![ib, ia, fb, fa]);

            b.set_block(exit);
            b.ret_void();
        }
        let mf = select_function(&func);
        let body_mb = find_block(&mf, "body");
        let gp_moves = count_vreg_moves(body_mb, ArmOpcode::MovReg);
        let fp_moves = count_vreg_moves(body_mb, ArmOpcode::FmovReg);
        assert_eq!(
            gp_moves, 3,
            "GP 2-cycle should emit 3 MovReg, got {}: {:#?}",
            gp_moves, body_mb.insts,
        );
        assert_eq!(
            fp_moves, 3,
            "FP 2-cycle should emit 3 FmovReg, got {}: {:#?}",
            fp_moves, body_mb.insts,
        );
    }

    #[test]
    fn logical_arrays_use_default_kind_storage_for_stack_slots() {
        assert_eq!(alloca_size(&IrType::Array(Box::new(IrType::Bool), 3)), 12);
        assert_eq!(
            alloca_size(&IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 3)),
            12
        );
    }
}
