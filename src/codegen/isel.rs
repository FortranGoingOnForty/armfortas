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
            Some(RegClass::Gp32) if gp_idx < 8 => {
                // 32-bit VALUE param: arrives in w-register.
                mf.block_mut(MBlockId(0)).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::VReg(*vreg),
                        MachineOperand::PhysReg(PhysReg::Gp32(gp_idx)),
                    ],
                    def: Some(*vreg),
                });
                gp_idx += 1;
            }
            _ if gp_idx < 8 => {
                // 64-bit or pointer param: arrives in x-register.
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
            let class = type_to_reg_class(&bp.ty);
            let vreg = mf.new_vreg(class);
            ctx.value_map.insert(bp.id, vreg);
        }
        for inst in &block.insts {
            // Allocas are special: they're handled by Phase 1
            // (stack-slot allocation). They don't get vregs.
            if matches!(inst.kind, InstKind::Alloca(_)) { continue; }
            // Void-typed insts (Store, RuntimeCall returning void,
            // etc.) don't produce a usable value.
            if matches!(inst.ty, IrType::Void) { continue; }
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

    // Phase 4b: select instructions and terminators for each block.
    for block in &func.blocks {
        let mb_id = ctx.block_map[&block.id];

        for inst in &block.insts {
            select_inst(&mut mf, &mut ctx, mb_id, inst);
        }

        if let Some(term) = &block.terminator {
            select_terminator(&mut mf, &mut ctx, mb_id, term, block);
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
    /// IR BlockId → its block params. Snapshotted before phase 4b
    /// so terminator selection can read each target's params
    /// without re-borrowing the function while &mut MachineFunction
    /// is held. Cloning just the param vec is dramatically cheaper
    /// than cloning the whole BasicBlock — instructions can be in
    /// the thousands, params are typically 0-3.
    block_params: HashMap<BlockId, Vec<BlockParam>>,
}

impl ISelCtx {
    fn new() -> Self {
        Self {
            value_map: HashMap::new(),
            block_map: HashMap::new(),
            alloca_offsets: HashMap::new(),
            block_params: HashMap::new(),
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
                val.0, class,
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
                val.0, self.value_map.len(),
            )
        })
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

        // ---- Select (CSEL) ----
        InstKind::Select(cond, tv, fv) => {
            let cond_reg = ctx.lookup_vreg(*cond);
            let true_reg = ctx.lookup_vreg(*tv);
            let false_reg = ctx.lookup_vreg(*fv);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);

            // Compare cond to zero: CMP cond, #0
            mf.block_mut(mb).insts.push(MachineInst {
                opcode: ArmOpcode::CmpImm,
                operands: vec![MachineOperand::VReg(cond_reg), MachineOperand::Imm(0)],
                def: None,
            });

            // Select based on condition: CSEL/FCSEL dest, true, false, NE
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
                    MachineOperand::Cond(ArmCond::Ne),
                ],
                def: Some(dest),
            });
        }

        // ---- Float: fabs, fsqrt ----
        InstKind::FAbs(a) => {
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            let opcode = if class == RegClass::Fp64 { ArmOpcode::FabsD } else { ArmOpcode::FabsS };
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
            let opcode = if class == RegClass::Fp64 { ArmOpcode::FsqrtD } else { ArmOpcode::FsqrtS };
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

        // ---- Integer extend/truncate ----
        InstKind::IntExtend(a, _target_width, signed) => {
            let src = ctx.lookup_vreg(*a);
            let class = type_to_reg_class(&inst.ty);
            let dest = ctx.get_vreg(mf, inst.id, class);
            if *signed {
                // SXTW Xd, Wn — sign-extend 32-bit to 64-bit.
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::Sxtw,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::VReg(src),
                    ],
                    def: Some(dest),
                });
            } else {
                // Zero-extend: MOV Xd, Wn (ARM64 implicitly zero-extends W→X).
                mf.block_mut(mb).insts.push(MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::VReg(dest),
                        MachineOperand::VReg(src),
                    ],
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
                operands: vec![
                    MachineOperand::VReg(dest),
                    MachineOperand::VReg(src),
                ],
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
) {
    let _ = src_block; // used implicitly via `term`'s args; kept for clarity
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
        Terminator::CondBranch { cond, true_dest, true_args, false_dest, false_args } => {
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
    if args.is_empty() { return; }

    // Look up the target block's param vregs in the same order
    // they appear in the IR (which is also the order they were
    // allocated in Phase 4a, so the i-th arg corresponds to the
    // i-th param).
    let target_params = ctx.block_params.get(&target_block)
        .expect("isel: branch target not in block_params snapshot");
    if target_params.len() != args.len() {
        // Verifier should reject this — but if it leaks through
        // we want a clear panic, not silent corruption.
        panic!(
            "isel: branch arg count {} ≠ target block param count {}",
            args.len(), target_params.len()
        );
    }

    // Build the list of (dst_vreg, src_vreg) pairs.
    let mut pending: Vec<(VRegId, VRegId)> = Vec::with_capacity(args.len());
    for (arg, bp) in args.iter().zip(target_params.iter()) {
        let dst = ctx.lookup_vreg(bp.id);
        let src = ctx.lookup_vreg(*arg);
        if dst != src {
            pending.push((dst, src));
        }
    }
    if pending.is_empty() { return; }

    // Helper to look up a vreg's RegClass via mf.vregs.
    fn class_of(mf: &MachineFunction, v: VRegId) -> RegClass {
        mf.vregs.iter().find(|r| r.id == v)
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
            operands: vec![
                MachineOperand::VReg(dst),
                MachineOperand::VReg(src),
            ],
            def: Some(dst),
        });
    };

    // Iteratively emit safe moves; break cycles via scratch.
    while !pending.is_empty() {
        // Find a safe copy: dst is not the src of any other
        // pending copy.
        let safe_idx = (0..pending.len()).find(|&i| {
            let (d, _) = pending[i];
            !pending.iter().enumerate().any(|(j, &(_, s))| j != i && s == d)
        });

        if let Some(idx) = safe_idx {
            let (d, s) = pending.remove(idx);
            emit_move(mf, mb, d, s);
        } else {
            // No safe copy → all remaining are part of a cycle.
            // Break by routing the first copy through a scratch.
            let (d, s) = pending[0];
            let class = class_of(mf, s);
            let temp = mf.new_vreg(class);
            emit_move(mf, mb, temp, s);
            pending[0] = (d, temp);
            // The next iteration's safe-search will find this
            // copy's `temp` source has no other readers, making
            // (d, temp) safe to emit.
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
        block.insts.iter()
            .filter(|i| i.opcode == opcode)
            .filter(|i| {
                // True parallel copies are VReg → VReg.
                matches!(i.operands.first(), Some(MachineOperand::VReg(_)))
                    && matches!(i.operands.get(1), Some(MachineOperand::VReg(_)))
            })
            .count()
    }

    fn find_block<'a>(mf: &'a MachineFunction, contains: &str) -> &'a MachineBlock {
        mf.blocks.iter()
            .find(|b| b.label.contains(contains))
            .unwrap_or_else(|| panic!(
                "no machine block containing '{}' (have: {:?})",
                contains,
                mf.blocks.iter().map(|b| &b.label).collect::<Vec<_>>(),
            ))
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
}
