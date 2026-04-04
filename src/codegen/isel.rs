//! Instruction selection — translate SSA IR to Machine IR.
//!
//! Maps each IR instruction to one or more ARM64 machine instructions.
//! Uses virtual registers throughout; physical register assignment
//! happens in the register allocator (Sprint 21).
//!
//! Strategy: naive spill-everything. Every vreg lives on the stack.
//! Load before use, store after def. Correct but slow — optimized later.

use std::collections::HashMap;
use crate::ir::types::*;
use crate::ir::inst::*;
use super::mir::*;

/// Select machine instructions for an entire IR module.
pub fn select_module(module: &Module) -> Vec<MachineFunction> {
    module.functions.iter().map(select_function).collect()
}

/// Select machine instructions for one IR function.
pub fn select_function(func: &Function) -> MachineFunction {
    let mut mf = MachineFunction::new(func.name.clone());
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
    ctx.block_map.insert(func.entry, MBlockId(0));
    for block in &func.blocks {
        if block.id != func.entry {
            let mb_id = mf.new_block(&block.name);
            ctx.block_map.insert(block.id, mb_id);
        }
    }

    // Phase 2.5: handle incoming parameters.
    // Create a vreg for each param and map its ValueId.
    // The physical register save happens after the prologue.
    let mut param_info: Vec<(ValueId, VRegId)> = Vec::new();
    for param in &func.params {
        let class = type_to_reg_class(&param.ty);
        let vreg = mf.new_vreg(class);
        ctx.value_map.insert(param.id, vreg);
        param_info.push((param.id, vreg));
    }

    // Phase 3: emit prologue in entry block.
    emit_prologue(&mut mf, MBlockId(0));

    // Phase 3.5: move incoming argument registers into param vregs.
    // Dispatch by register class: GP args from x0-x7, FP args from d0-d7.
    let mut gp_idx: u8 = 0;
    let mut fp_idx: u8 = 0;
    for (_param_id, vreg) in &param_info {
        let class = mf.vregs.iter().find(|v| v.id == *vreg).map(|v| v.class);
        match class {
            Some(RegClass::Fp64) if fp_idx < 8 => {
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::FmovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Fp(fp_idx)),
                    ],
                    def: Some(*vreg),
                });
                fp_idx += 1;
            }
            Some(RegClass::Fp32) if fp_idx < 8 => {
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::FmovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Fp32(fp_idx)),
                    ],
                    def: Some(*vreg),
                });
                fp_idx += 1;
            }
            _ if gp_idx < 8 => {
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Gp(gp_idx)),
                    ],
                    def: Some(*vreg),
                });
                gp_idx += 1;
            }
            _ => {} // 9+ args: stack passing not yet implemented
        }
    }

    // Phase 4: select instructions for each block.
    for block in &func.blocks {
        let mb_id = ctx.block_map[&block.id];
        // Block params → allocate vregs.
        for bp in &block.params {
            let class = type_to_reg_class(&bp.ty);
            let vreg = mf.new_vreg(class);
            ctx.value_map.insert(bp.id, vreg);
        }

        for inst in &block.insts {
            select_inst(&mut mf, &mut ctx, mb_id, inst);
        }

        if let Some(term) = &block.terminator {
            select_terminator(&mut mf, &mut ctx, mb_id, term);
        }
    }

    mf
}

/// Instruction selection context.
struct ISelCtx {
    /// IR ValueId → MIR VRegId.
    value_map: HashMap<ValueId, VRegId>,
    /// IR BlockId → MIR MBlockId.
    block_map: HashMap<BlockId, MBlockId>,
    /// IR alloca ValueId → stack frame offset.
    alloca_offsets: HashMap<ValueId, i32>,
}

impl ISelCtx {
    fn new() -> Self {
        Self {
            value_map: HashMap::new(),
            block_map: HashMap::new(),
            alloca_offsets: HashMap::new(),
        }
    }

    /// Get the vreg for an IR value, or create one if needed.
    fn get_vreg(&mut self, mf: &mut MachineFunction, val: ValueId, class: RegClass) -> VRegId {
        if let Some(&vreg) = self.value_map.get(&val) {
            return vreg;
        }
        let vreg = mf.new_vreg(class);
        self.value_map.insert(val, vreg);
        vreg
    }

    /// Get the vreg for an IR value, assuming it was already mapped.
    fn lookup_vreg(&self, val: ValueId) -> VRegId {
        *self.value_map.get(&val)
            .unwrap_or_else(|| panic!("isel: unmapped IR value %{}", val.0))
    }

    /// Get machine block for an IR block.
    fn lookup_block(&self, block: BlockId) -> MBlockId {
        *self.block_map.get(&block).unwrap_or(&MBlockId(0))
    }
}

/// Select machine instructions for a single IR instruction.
fn select_inst(mf: &mut MachineFunction, ctx: &mut ISelCtx, mb: MBlockId, inst: &Inst) {
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
            let class = type_to_reg_class(&inst.ty);
            let _dest = ctx.get_vreg(mf, inst.id, class);
            // Undef: just allocate the vreg, no instruction needed.
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
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::VReg(va),
                ],
                def: Some(dest),
            });
        }

        // ---- Float arithmetic ----
        InstKind::FAdd(a, b) => emit_float_binop(mf, ctx, mb, inst, &inst.ty, *a, *b, ArmOpcode::FaddS, ArmOpcode::FaddD),
        InstKind::FSub(a, b) => emit_float_binop(mf, ctx, mb, inst, &inst.ty, *a, *b, ArmOpcode::FsubS, ArmOpcode::FsubD),
        InstKind::FMul(a, b) => emit_float_binop(mf, ctx, mb, inst, &inst.ty, *a, *b, ArmOpcode::FmulS, ArmOpcode::FmulD),
        InstKind::FDiv(a, b) => emit_float_binop(mf, ctx, mb, inst, &inst.ty, *a, *b, ArmOpcode::FdivS, ArmOpcode::FdivD),
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
                IrType::Float(FloatWidth::F32) => ("powf", PhysReg::Fp32(0), PhysReg::Fp32(1), PhysReg::Fp32(0)),
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
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp32);
            let va = ctx.lookup_vreg(*a);
            let vb = ctx.lookup_vreg(*b);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::CmpReg,
                operands: vec![MachineOperand::VReg(va), MachineOperand::VReg(vb)],
                def: None,
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Cset,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::Cond(cmp_to_arm_cond(*op)),
                ],
                def: Some(dest),
            });
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
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::FCset,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::Cond(fcmp_to_arm_cond(*op)),
                ],
                def: Some(dest),
            });
        }

        // ---- Logic ----
        InstKind::And(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::AndReg, *a, *b),
        InstKind::Or(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::OrrReg, *a, *b),
        InstKind::Not(a) => {
            // NOT = ORN dest, XZR, src  (MVN alias)
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let va = ctx.lookup_vreg(*a);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::OrnReg,
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::PhysReg(PhysReg::Xzr),
                    MachineOperand::VReg(va),
                ],
                def: Some(dest),
            });
        }

        // ---- Bitwise ----
        InstKind::BitAnd(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::AndReg, *a, *b),
        InstKind::BitOr(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::OrrReg, *a, *b),
        InstKind::BitXor(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::EorReg, *a, *b),
        InstKind::Shl(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::LslReg, *a, *b),
        InstKind::AShr(a, b) => emit_binop(mf, ctx, mb, inst, ArmOpcode::AsrReg, *a, *b),

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
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let is_fp = matches!(class, RegClass::Fp32 | RegClass::Fp64);
            let opcode = if is_fp { ArmOpcode::LdrFpImm } else { ArmOpcode::LdrImm };

            // If addr is an alloca, load directly from the frame slot.
            if let Some(&offset) = ctx.alloca_offsets.get(addr) {
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::FrameSlot(offset),
                    ],
                    def: Some(dest),
                });
            } else {
                let base = ctx.lookup_vreg(*addr);
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::VReg(base),
                        MachineOperand::Imm(0),
                    ],
                    def: Some(dest),
                });
            }
        }

        InstKind::Store(val, addr) => {
            let val_vreg = ctx.lookup_vreg(*val);
            let val_class = mf.vregs.iter().find(|v| v.id == val_vreg).map(|v| v.class);
            let is_fp = matches!(val_class, Some(RegClass::Fp32) | Some(RegClass::Fp64));
            let opcode = if is_fp { ArmOpcode::StrFpImm } else { ArmOpcode::StrImm };

            if let Some(&offset) = ctx.alloca_offsets.get(addr) {
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode,
                    operands: vec![
                        MachineOperand::VReg(val_vreg),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::FrameSlot(offset),
                    ],
                    def: None,
                });
            } else {
                let base = ctx.lookup_vreg(*addr);
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode,
                    operands: vec![
                        MachineOperand::VReg(val_vreg),
                        MachineOperand::VReg(base),
                        MachineOperand::Imm(0),
                    ],
                    def: None,
                });
            }
        }

        InstKind::GetElementPtr(base, indices) => {
            // GEP: base + index * elem_size
            let dest = ctx.get_vreg(mf, inst.id, RegClass::Gp64);
            let base_vreg = ctx.lookup_vreg(*base);

            // Determine element size from the GEP result type (Ptr<elem_ty>).
            let elem_size = match &inst.ty {
                IrType::Ptr(inner) => alloca_size(inner) as i64,
                _ => 4, // fallback
            };

            if let Some(idx) = indices.first() {
                let idx_vreg = ctx.lookup_vreg(*idx);
                let tmp = mf.new_vreg(RegClass::Gp64);
                emit_const_int(mf, mb, tmp, elem_size, IntWidth::I64);
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
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::VReg(base_vreg),
                    ],
                    def: Some(dest),
                });
            }
        }

        // ---- Calls ----
        InstKind::Call(..) | InstKind::RuntimeCall(..) => {
            let (label, args) = match &inst.kind {
                InstKind::Call(FuncRef::External(name), args) => (name.clone(), args.as_slice()),
                InstKind::Call(FuncRef::Internal(idx), args) => (format!("_func_{}", idx), args.as_slice()),
                InstKind::RuntimeCall(rf, args) => (runtime_func_symbol(rf), args.as_slice()),
                _ => unreachable!(),
            };

            // Move arguments to physical registers per AAPCS64.
            // Integer/pointer args → x0-x7, float args → d0-d7.
            let mut gp_idx: u8 = 0;
            let mut fp_idx: u8 = 0;
            for &arg_val in args {
                let arg_vreg = ctx.lookup_vreg(arg_val);
                let arg_class = mf.vregs.iter().find(|v| v.id == arg_vreg).map(|v| v.class);
                match arg_class {
                    Some(RegClass::Fp64) => {
                        if fp_idx < 8 {
                            mf.block_mut(mb).insts.push(MachineInst {
                                opcode: ArmOpcode::FmovReg,
                                operands: vec![
                                    MachineOperand::PhysReg(PhysReg::Fp(fp_idx)),
                                    MachineOperand::VReg(arg_vreg),
                                ],
                                def: None,
                            });
                            fp_idx += 1;
                        }
                    }
                    Some(RegClass::Fp32) => {
                        if fp_idx < 8 {
                            mf.block_mut(mb).insts.push(MachineInst {
                                opcode: ArmOpcode::FmovReg,
                                operands: vec![
                                    MachineOperand::PhysReg(PhysReg::Fp32(fp_idx)),
                                    MachineOperand::VReg(arg_vreg),
                                ],
                                def: None,
                            });
                            fp_idx += 1;
                        }
                    }
                    Some(RegClass::Gp32) => {
                        if gp_idx < 8 {
                            mf.block_mut(mb).insts.push(MachineInst {
                                opcode: ArmOpcode::MovReg,
                                operands: vec![
                                    MachineOperand::PhysReg(PhysReg::Gp32(gp_idx)),
                                    MachineOperand::VReg(arg_vreg),
                                ],
                                def: None,
                            });
                            gp_idx += 1;
                        }
                    }
                    _ => {
                        // Gp64 or unknown — use 64-bit register.
                        if gp_idx < 8 {
                            mf.block_mut(mb).insts.push(MachineInst {
                                opcode: ArmOpcode::MovReg,
                                operands: vec![
                                    MachineOperand::PhysReg(PhysReg::Gp(gp_idx)),
                                    MachineOperand::VReg(arg_vreg),
                                ],
                                def: None,
                            });
                            gp_idx += 1;
                        }
                    }
                }
            }

            // Emit BL.
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Bl,
                operands: vec![MachineOperand::Extern(label)],
                def: None,
            });

            // Capture return value from x0 or d0.
            if inst.ty != IrType::Void {
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
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::PhysReg(src_reg),
                    ],
                    def: Some(dest),
                });
            } else {
                // Still allocate a vreg for void calls (keeps value_map consistent).
                ctx.get_vreg(mf, inst.id, RegClass::Gp64);
            }
        }

        // Remaining: IntExtend, IntTrunc, ExtractField, InsertField — emit MOV as placeholder.
        _ => {
            let class = type_to_reg_class(&inst.ty);
            let _dest = ctx.get_vreg(mf, inst.id, class);
            // Placeholder NOP for unimplemented instructions.
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            });
        }
    }
}

/// Select machine instructions for a terminator.
fn select_terminator(mf: &mut MachineFunction, ctx: &mut ISelCtx, mb: MBlockId, term: &Terminator) {
    match term {
        Terminator::Return(None) => {
            emit_epilogue(mf, mb);
        }
        Terminator::Return(Some(val)) => {
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
        Terminator::Branch(dest, _args) => {
            let target = ctx.lookup_block(*dest);
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::B,
                operands: vec![MachineOperand::BlockRef(target)],
                def: None,
            });
        }
        Terminator::CondBranch { cond, true_dest, false_dest, .. } => {
            let cond_vreg = ctx.lookup_vreg(*cond);
            let true_mb = ctx.lookup_block(*true_dest);
            let false_mb = ctx.lookup_block(*false_dest);

            // CMP cond, #0; B.NE true_label; B false_label
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::CmpImm,
                operands: vec![MachineOperand::VReg(cond_vreg), MachineOperand::Imm(0)],
                def: None,
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::BCond,
                operands: vec![
                    MachineOperand::Cond(ArmCond::Ne),
                    MachineOperand::BlockRef(true_mb),
                ],
                def: None,
            });
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::B,
                operands: vec![MachineOperand::BlockRef(false_mb)],
                def: None,
            });
        }
        Terminator::Switch { selector, cases, default } => {
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

/// Emit a constant integer using movz/movk sequence.
/// Respects width: 32-bit values mask to 32 bits and only emit shifts 0/16.
fn emit_const_int(mf: &mut MachineFunction, mb: MBlockId, dest: VRegId, val: i64, width: IntWidth) {
    let is_32 = matches!(width, IntWidth::I8 | IntWidth::I16 | IntWidth::I32);
    // Mask to the appropriate width to prevent sign-extension artifacts.
    let uval = if is_32 { (val as u32) as u64 } else { val as u64 };
    let max_shift = if is_32 { 2 } else { 4 }; // 2 chunks for 32-bit, 4 for 64-bit

    if uval == 0 {
        let zr = if is_32 { PhysReg::Wzr } else { PhysReg::Xzr };
        mf.block_mut(mb).insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::VReg(dest),
                MachineOperand::PhysReg(zr),
            ],
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
            let opcode = if first { ArmOpcode::Movz } else { ArmOpcode::Movk };
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
            operands: vec![
                MachineOperand::VReg(dest),
                MachineOperand::PhysReg(zr),
            ],
            def: Some(dest),
        });
    }
}

/// Emit a register-register binary op.
fn emit_binop(mf: &mut MachineFunction, ctx: &mut ISelCtx, mb: MBlockId, inst: &Inst, opcode: ArmOpcode, a: ValueId, b: ValueId) {
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
fn emit_float_binop(mf: &mut MachineFunction, ctx: &mut ISelCtx, mb: MBlockId, inst: &Inst, ty: &IrType, a: ValueId, b: ValueId, op_s: ArmOpcode, op_d: ArmOpcode) {
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
fn type_to_reg_class(ty: &IrType) -> RegClass {
    match ty {
        IrType::Float(FloatWidth::F32) => RegClass::Fp32,
        IrType::Float(FloatWidth::F64) => RegClass::Fp64,
        IrType::Int(IntWidth::I8) | IrType::Int(IntWidth::I16) |
        IrType::Int(IntWidth::I32) | IrType::Bool => RegClass::Gp32,
        _ => RegClass::Gp64,
    }
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

/// Map IR comparison op to ARM64 condition code (for float FCMP).
fn fcmp_to_arm_cond(op: CmpOp) -> ArmCond {
    match op {
        CmpOp::Eq => ArmCond::Eq,
        CmpOp::Ne => ArmCond::Ne,
        CmpOp::Lt => ArmCond::Mi,  // minus flag for less-than
        CmpOp::Le => ArmCond::Ls,  // unsigned LE maps to float LE
        CmpOp::Gt => ArmCond::Gt,
        CmpOp::Ge => ArmCond::Ge,
    }
}

/// Compute allocation size for an IR type.
fn alloca_size(ty: &IrType) -> u32 {
    match ty {
        IrType::Void => 0,
        IrType::Bool => 4,  // use 4 bytes for alignment
        IrType::Int(w) => w.bytes(),
        IrType::Float(w) => w.bytes(),
        IrType::Ptr(_) => 8,
        IrType::Array(elem, count) => {
            let elem_size = alloca_size(elem);
            elem_size * (*count as u32)
        }
        IrType::FuncPtr(_) => 8,
        IrType::Struct(_) => 8, // placeholder
    }
}

/// Get the symbol name for a runtime function.
/// Get the C-level symbol name for a runtime function.
/// The emitter adds the Mach-O `_` prefix when emitting assembly.
fn runtime_func_symbol(rf: &RuntimeFunc) -> String {
    match rf {
        RuntimeFunc::PrintInt => "afs_print_int".into(),
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
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::AddReg));
    }

    #[test]
    fn select_icmp() {
        let mf = select_simple(|b| {
            let x = b.const_i32(5);
            let y = b.const_i32(10);
            let _c = b.icmp(CmpOp::Lt, x, y);
            b.ret_void();
        });
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::CmpReg));
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::Cset));
    }

    #[test]
    fn select_fadd() {
        let mf = select_simple(|b| {
            let x = b.const_f64(1.0);
            let y = b.const_f64(2.0);
            let _z = b.fadd(x, y);
            b.ret_void();
        });
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::FaddD));
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
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::SubImm));
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::StrImm));
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
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::BCond));
    }

    #[test]
    fn select_call() {
        let mf = select_simple(|b| {
            b.runtime_call(
                crate::ir::inst::RuntimeFunc::PrintInt,
                vec![],
                IrType::Void,
            );
            b.ret_void();
        });
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::Bl));
    }

    #[test]
    fn prologue_and_epilogue() {
        let mf = select_simple(|b| {
            b.ret_void();
        });
        let insts = &mf.blocks[0].insts;
        assert_eq!(insts[0].opcode, ArmOpcode::StpPre, "first inst should be STP (prologue)");
        assert_eq!(insts[1].opcode, ArmOpcode::AddImm, "second inst should be ADD FP, SP, #offset");
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::Ret), "should have RET");
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
            i.opcode == ArmOpcode::MovReg &&
            i.operands.iter().any(|o| matches!(o,
                MachineOperand::PhysReg(PhysReg::Xzr) |
                MachineOperand::PhysReg(PhysReg::Wzr)
            ))
        });
        assert!(has_mov_zr, "const 0 should use MOV from XZR or WZR");
    }
}
