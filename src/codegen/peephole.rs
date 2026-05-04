//! Post-isel peephole optimizations.
//!
//! Runs on `MachineFunction` after instruction selection and before
//! register allocation. Virtual registers are still in use.
//!
//! ## FMADD / FMSUB fusion
//!
//! ARM64 has fused multiply-add instructions that are both faster and
//! more accurate (single rounding) than a separate fmul + fadd pair.
//!
//! Matched patterns and their replacement:
//!
//! | Pattern                               | Replacement                 | ARM64 opcode |
//! |---------------------------------------|-----------------------------|--------------|
//! | `fadd(fmul(a, b), c)` or commuted    | `result = c + a*b`          | FMADD        |
//! | `fsub(c, fmul(a, b))`                | `result = c - a*b`          | FMSUB        |
//! | `fsub(fmul(a, b), c)`                | `result = a*b - c`          | FNMSUB       |
//!
//! ## MADD / MSUB fusion (integer)
//!
//! Same shape as the FP family but for integer ops. ARM64's `madd` /
//! `msub` are 3-source integer multiply-add/subtract that fuse `mul`
//! followed by `add`/`sub` in a single instruction. We never spelled
//! these from `isel.rs` directly because the IR exposes `IAdd(IMul, c)`
//! as two separate opcodes and isel emits them naively.
//!
//! Matched patterns:
//!
//! | Pattern                               | Replacement                 | ARM64 opcode |
//! |---------------------------------------|-----------------------------|--------------|
//! | `add(mul(a, b), c)` or commuted      | `result = c + a*b`          | MADD         |
//! | `sub(c, mul(a, b))`                  | `result = c - a*b`          | MSUB         |
//!
//! Note: `sub(mul(a,b), c)` (i.e. `a*b - c`) has no single-instruction
//! ARM64 form for integer ops — `msub` computes `Xa - Xn*Xm`, not the
//! other order, and there is no integer analogue of `fnmsub`. We leave
//! that pattern as-is.
//!
//! ## Preconditions
//!
//! * The MUL/FMUL result is used exactly once (the subsequent ADD/SUB).
//! * Both instructions are in the same machine block.
//!
//! After fusion the multiply is removed and the add/sub is replaced
//! with the three-source instruction.

use super::mir::{ArmOpcode, MachineFunction, MachineInst, MachineOperand, VRegId};
use std::collections::HashMap;

/// Run all peephole passes on a machine function. Iterates to fixpoint
/// so newly-exposed patterns (e.g., a strength-reduction that produces
/// an `add x, x, x` fed by a mul-add candidate) compose without
/// requiring callers to re-invoke us. Capped at a small number of
/// rounds — passes are monotone (only remove or simplify), so fixpoint
/// is bounded by instruction count and an 8-round ceiling is plenty in
/// practice while still surfacing pathological non-convergence as an
/// internal-error rather than a hang.
pub fn run_peephole(mf: &mut MachineFunction) {
    const MAX_ROUNDS: u32 = 8;
    for _ in 0..MAX_ROUNDS {
        let before = total_inst_count(mf);
        // Scaled-addressing fusion first so it can consume the
        // Mul + AddReg pair before madd_fusion would collapse them
        // into a Madd. A single load-with-shift beats a fused madd
        // followed by a plain ldr.
        scaled_addressing_fusion(mf);
        fma_fusion(mf);
        madd_fusion(mf);
        if total_inst_count(mf) == before {
            return;
        }
    }
}

fn total_inst_count(mf: &MachineFunction) -> usize {
    mf.blocks.iter().map(|b| b.insts.len()).sum()
}

/// FMADD/FMSUB/FNMSUB fusion.
fn fma_fusion(mf: &mut MachineFunction) {
    for mb_idx in 0..mf.blocks.len() {
        fma_fuse_block(mf, mb_idx);
    }
}

fn fma_fuse_block(mf: &mut MachineFunction, mb_idx: usize) {
    let block = &mf.blocks[mb_idx];

    // Count uses of each defined VReg within this block.
    // A fmul result is fusable only when its use count == 1 (the fadd/fsub).
    let mut use_count: HashMap<VRegId, usize> = HashMap::new();
    for inst in &block.insts {
        for op in &inst.operands {
            if let MachineOperand::VReg(v) = op {
                *use_count.entry(*v).or_insert(0) += 1;
            }
        }
    }
    // Subtract the self-def (operands[0] is the dest, counted in use_count
    // but it's a def not a use). For a 3-operand FmulS [dest, src0, src1]:
    // dest appears once in operands but it's the definition, not a use.
    // Re-compute: only operands[1..] are uses.
    let mut use_count: HashMap<VRegId, usize> = HashMap::new();
    for inst in &block.insts {
        // Operands beyond index 0 are inputs (index 0 is the output dest).
        for op in inst.operands.iter().skip(1) {
            if let MachineOperand::VReg(v) = op {
                *use_count.entry(*v).or_insert(0) += 1;
            }
        }
    }

    // Map: vreg defined by a fmul instruction → (block-instruction-index, precision)
    #[derive(Clone, Copy)]
    enum Prec {
        S,
        D,
    }
    let mut fmul_defs: HashMap<VRegId, (usize, Prec)> = HashMap::new();

    for (i, inst) in block.insts.iter().enumerate() {
        match inst.opcode {
            ArmOpcode::FmulS => {
                if let Some(d) = inst.def {
                    fmul_defs.insert(d, (i, Prec::S));
                }
            }
            ArmOpcode::FmulD => {
                if let Some(d) = inst.def {
                    fmul_defs.insert(d, (i, Prec::D));
                }
            }
            _ => {}
        }
    }

    // Collect fusions to apply (instruction index → replacement info).
    // Each entry: (fadd/fsub_idx, fmul_idx, new_opcode, new_operands)
    struct FusionPlan {
        add_idx: usize,
        mul_idx: usize,
        new_opcode: ArmOpcode,
        // operands for the fused inst: [dest, Fn, Fm, Fa]
        n: VRegId, // multiply lhs
        m: VRegId, // multiply rhs
        a: VRegId, // addend / subtrahend (accumulate register)
    }
    let mut plans: Vec<FusionPlan> = Vec::new();

    let block = &mf.blocks[mb_idx];
    for (i, inst) in block.insts.iter().enumerate() {
        let (is_add, is_sub, prec_s) = match inst.opcode {
            ArmOpcode::FaddS => (true, false, true),
            ArmOpcode::FaddD => (true, false, false),
            ArmOpcode::FsubS => (false, true, true),
            ArmOpcode::FsubD => (false, true, false),
            _ => continue,
        };
        if inst.operands.len() < 3 {
            continue;
        }

        // operands: [dest, src0, src1]
        let src0 = match &inst.operands[1] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };
        let src1 = match &inst.operands[2] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };

        // Check if src0 is a single-use fmul result.
        let try_fuse_src0 = fmul_defs
            .get(&src0)
            .filter(|_| use_count.get(&src0).copied().unwrap_or(0) == 1);
        // Check if src1 is a single-use fmul result.
        let try_fuse_src1 = if is_add {
            fmul_defs
                .get(&src1)
                .filter(|_| use_count.get(&src1).copied().unwrap_or(0) == 1)
        } else {
            None // fsub(c, fmul(a,b)): src0=c, src1=fmul → handled separately
        };

        if is_add {
            // fadd(fmul(a,b), c) → FMADD(a, b, c)
            if let Some(&(mul_idx, _)) = try_fuse_src0 {
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FmaddS
                } else {
                    ArmOpcode::FmaddD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src1,
                });
            } else if let Some(&(mul_idx, _)) = try_fuse_src1 {
                // fadd(c, fmul(a,b)) → FMADD(a, b, c)  [commuted]
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FmaddS
                } else {
                    ArmOpcode::FmaddD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src0,
                });
            }
        } else if is_sub {
            // fsub(fmul(a,b), c) → FNMSUB(a, b, c)  [result = a*b - c]
            if let Some(&(mul_idx, _)) = try_fuse_src0 {
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FnmsubS
                } else {
                    ArmOpcode::FnmsubD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src1,
                });
            }
            // fsub(c, fmul(a,b)) → FMSUB(a, b, c)  [result = c - a*b]
            // src0=c, src1=fmul_result
            let try_fuse_sub1 = fmul_defs
                .get(&src1)
                .filter(|_| use_count.get(&src1).copied().unwrap_or(0) == 1);
            if let Some(&(mul_idx, _)) = try_fuse_sub1 {
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let opcode = if prec_s {
                    ArmOpcode::FmsubS
                } else {
                    ArmOpcode::FmsubD
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: opcode,
                    n,
                    m,
                    a: src0,
                });
            }
        }
    }

    if plans.is_empty() {
        return;
    }

    // Apply plans in reverse order of add_idx to keep indices stable.
    plans.sort_by_key(|plan| std::cmp::Reverse(plan.add_idx));

    // Collect mul_idxs to remove.
    let mut remove_idxs: std::collections::HashSet<usize> =
        plans.iter().map(|p| p.mul_idx).collect();

    // Rewrite the block.
    let block = &mut mf.blocks[mb_idx];
    for plan in &plans {
        let dest = block.insts[plan.add_idx].def;
        let dest_op = block.insts[plan.add_idx].operands[0].clone();
        block.insts[plan.add_idx] = MachineInst {
            opcode: plan.new_opcode,
            operands: vec![
                dest_op,
                MachineOperand::VReg(plan.n),
                MachineOperand::VReg(plan.m),
                MachineOperand::VReg(plan.a),
            ],
            def: dest,
        };
    }
    let mut idx = 0usize;
    block.insts.retain(|_| {
        let keep = !remove_idxs.remove(&idx);
        idx += 1;
        keep
    });
}

/// Integer MADD/MSUB fusion. Mirrors `fma_fusion` for `Mul + AddReg`
/// and `Mul + SubReg`. Matched patterns:
///
/// * `AddReg(Mul(a, b), c)`  →  `Madd(a, b, c)`
/// * `AddReg(c, Mul(a, b))`  →  `Madd(a, b, c)`  (commuted)
/// * `SubReg(c, Mul(a, b))`  →  `Msub(a, b, c)`  (only this order — there
///   is no integer FNMSUB analogue)
///
/// Preconditions match the FP version: same block, mul result has
/// exactly one use (the add/sub).
fn madd_fusion(mf: &mut MachineFunction) {
    for mb_idx in 0..mf.blocks.len() {
        madd_fuse_block(mf, mb_idx);
    }
}

fn madd_fuse_block(mf: &mut MachineFunction, mb_idx: usize) {
    let block = &mf.blocks[mb_idx];

    // Per-block use counts of values, *as inputs only*. operands[0] is
    // the def for AddReg/SubReg/Mul, so we skip index 0 to count true
    // uses.
    let mut use_count: HashMap<VRegId, usize> = HashMap::new();
    for inst in &block.insts {
        for op in inst.operands.iter().skip(1) {
            if let MachineOperand::VReg(v) = op {
                *use_count.entry(*v).or_insert(0) += 1;
            }
        }
    }

    // VReg defined by an integer Mul → block index of that mul.
    let mut mul_defs: HashMap<VRegId, usize> = HashMap::new();
    for (i, inst) in block.insts.iter().enumerate() {
        if inst.opcode == ArmOpcode::Mul {
            if let Some(d) = inst.def {
                mul_defs.insert(d, i);
            }
        }
    }

    struct FusionPlan {
        add_idx: usize,
        mul_idx: usize,
        new_opcode: ArmOpcode,
        // [dest, n, m, a]
        n: VRegId,
        m: VRegId,
        a: VRegId,
    }
    let mut plans: Vec<FusionPlan> = Vec::new();

    for (i, inst) in block.insts.iter().enumerate() {
        let (is_add, is_sub) = match inst.opcode {
            ArmOpcode::AddReg => (true, false),
            ArmOpcode::SubReg => (false, true),
            _ => continue,
        };
        if inst.operands.len() < 3 {
            continue;
        }
        let src0 = match &inst.operands[1] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };
        let src1 = match &inst.operands[2] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };

        let single_use = |v: VRegId| use_count.get(&v).copied().unwrap_or(0) == 1;

        if is_add {
            // AddReg(Mul(a, b), c): src0 is the mul result.
            if let Some(&mul_idx) = mul_defs.get(&src0) {
                if !single_use(src0) {
                    // mul result is read by something else too — can't fuse.
                } else {
                    let mul_inst = &block.insts[mul_idx];
                    let n = match &mul_inst.operands[1] {
                        MachineOperand::VReg(v) => *v,
                        _ => continue,
                    };
                    let m = match &mul_inst.operands[2] {
                        MachineOperand::VReg(v) => *v,
                        _ => continue,
                    };
                    plans.push(FusionPlan {
                        add_idx: i,
                        mul_idx,
                        new_opcode: ArmOpcode::Madd,
                        n,
                        m,
                        a: src1,
                    });
                    continue;
                }
            }
            // AddReg(c, Mul(a, b))  [commuted].
            if let Some(&mul_idx) = mul_defs.get(&src1) {
                if !single_use(src1) {
                    continue;
                }
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: ArmOpcode::Madd,
                    n,
                    m,
                    a: src0,
                });
            }
        } else if is_sub {
            // SubReg(c, Mul(a, b))  →  Msub. The other order
            // (`a*b - c`) has no integer fused form on ARM64.
            if let Some(&mul_idx) = mul_defs.get(&src1) {
                if !single_use(src1) {
                    continue;
                }
                let mul_inst = &block.insts[mul_idx];
                let n = match &mul_inst.operands[1] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                let m = match &mul_inst.operands[2] {
                    MachineOperand::VReg(v) => *v,
                    _ => continue,
                };
                plans.push(FusionPlan {
                    add_idx: i,
                    mul_idx,
                    new_opcode: ArmOpcode::Msub,
                    n,
                    m,
                    a: src0,
                });
            }
        }
    }

    if plans.is_empty() {
        return;
    }

    // Apply plans in reverse add_idx order to keep indices stable.
    plans.sort_by_key(|plan| std::cmp::Reverse(plan.add_idx));
    let mut remove_idxs: std::collections::HashSet<usize> =
        plans.iter().map(|p| p.mul_idx).collect();

    let block = &mut mf.blocks[mb_idx];
    for plan in &plans {
        let dest = block.insts[plan.add_idx].def;
        let dest_op = block.insts[plan.add_idx].operands[0].clone();
        block.insts[plan.add_idx] = MachineInst {
            opcode: plan.new_opcode,
            operands: vec![
                dest_op,
                MachineOperand::VReg(plan.n),
                MachineOperand::VReg(plan.m),
                MachineOperand::VReg(plan.a),
            ],
            def: dest,
        };
    }
    let mut idx = 0usize;
    block.insts.retain(|_| {
        let keep = !remove_idxs.remove(&idx);
        idx += 1;
        keep
    });
}

/// ARM64 scaled-register addressing fusion.
///
/// Rewrites the 4-instruction sequence emitted by GEP+Load/Store
/// lowering when `elem_size` is a power of two ≤ 8:
///
/// ```text
/// movz tmp, #elem_size, 0      ; (or const-materializing equivalent)
/// mul  scaled, idx, tmp
/// add  addr, base, scaled
/// ldr  dest, [addr]            ; LdrImm/LdrFpImm/StrImm/StrFpImm, offset 0
/// ```
///
/// into a single instruction:
///
/// ```text
/// ldr  dest, [base, idx, lsl #shift]
/// ```
///
/// Preconditions checked per fusion site:
/// * `tmp` has exactly one use (the mul) and is defined by a single
///   `Movz` whose chunk value ∈ {1, 2, 4, 8} with shift 0.
/// * `scaled` (the mul result) has exactly one use (the add).
/// * `addr` (the add result) has exactly one use (the load/store).
/// * The load/store's offset operand is `Imm(0)`.
/// * All four instructions live in the same machine block.
///
/// Multi-use intermediates (e.g. the same GEP feeds two loads) keep
/// the unfused form — duplicating the index calculation across each
/// consumer would increase code size, not decrease it.
fn scaled_addressing_fusion(mf: &mut MachineFunction) {
    // Compute use counts function-wide. The fold removes 3
    // instructions (Movz, Mul, AddReg) and rewrites a 4th. If any of
    // those defs is consumed in *another* block (cross-block live
    // value), we must not fold — the consumer would lose its
    // operand. A per-block count understates true uses when the
    // value is live-out, which is what the cross-block path needs to
    // reject. Counting once per function is cheap and correct.
    let use_count = compute_global_use_counts(mf);
    for mb_idx in 0..mf.blocks.len() {
        scaled_addressing_fuse_block(mf, mb_idx, &use_count);
    }
}

fn compute_global_use_counts(mf: &MachineFunction) -> HashMap<VRegId, usize> {
    let mut use_count: HashMap<VRegId, usize> = HashMap::new();
    for block in &mf.blocks {
        for inst in &block.insts {
            // operand[0] is the def for instructions that have one.
            let skip_first = inst.def.is_some();
            for (i, op) in inst.operands.iter().enumerate() {
                if skip_first && i == 0 {
                    continue;
                }
                if let MachineOperand::VReg(v) = op {
                    *use_count.entry(*v).or_insert(0) += 1;
                }
            }
        }
    }
    use_count
}

fn scaled_addressing_fuse_block(
    mf: &mut MachineFunction,
    mb_idx: usize,
    use_count: &HashMap<VRegId, usize>,
) {
    let block = &mf.blocks[mb_idx];

    // Map vreg → (block-local index of defining instruction, opcode).
    let mut defs: HashMap<VRegId, (usize, ArmOpcode)> = HashMap::new();
    for (i, inst) in block.insts.iter().enumerate() {
        if let Some(d) = inst.def {
            defs.insert(d, (i, inst.opcode));
        }
    }

    struct FusionPlan {
        ldst_idx: usize,
        add_idx: usize,
        mul_idx: usize,
        movz_idx: usize,
        new_opcode: ArmOpcode,
        // [dest, base, idx, Imm(shift)]
        dest_op: MachineOperand,
        dest_def: Option<VRegId>,
        base: VRegId,
        idx: VRegId,
        shift: i64,
    }
    let mut plans: Vec<FusionPlan> = Vec::new();

    for (i, inst) in block.insts.iter().enumerate() {
        let new_opcode = match inst.opcode {
            ArmOpcode::LdrImm => ArmOpcode::LdrReg,
            ArmOpcode::LdrFpImm => ArmOpcode::LdrFpReg,
            ArmOpcode::StrImm => ArmOpcode::StrReg,
            ArmOpcode::StrFpImm => ArmOpcode::StrFpReg,
            _ => continue,
        };
        // Format: [dest_or_src, base, offset]. Offset must be Imm(0).
        if inst.operands.len() < 3 {
            continue;
        }
        let offset_zero = matches!(&inst.operands[2], MachineOperand::Imm(0));
        if !offset_zero {
            continue;
        }
        let base_addr = match &inst.operands[1] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };

        // The base address must come from a single-use AddReg.
        let (add_idx, add_op) = match defs.get(&base_addr) {
            Some(&(idx, op)) => (idx, op),
            None => continue,
        };
        if add_op != ArmOpcode::AddReg {
            continue;
        }
        if use_count.get(&base_addr).copied().unwrap_or(0) != 1 {
            continue;
        }

        let add_inst = &block.insts[add_idx];
        if add_inst.operands.len() < 3 {
            continue;
        }
        let add_lhs = match &add_inst.operands[1] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };
        let add_rhs = match &add_inst.operands[2] {
            MachineOperand::VReg(v) => *v,
            _ => continue,
        };

        // The scaled-index operand: defined by a single-use Mul. Try
        // either commuted order — `add base, scaled` or `add scaled,
        // base` (isel emits the former, but be defensive).
        let try_pair = |scaled: VRegId, base: VRegId| -> Option<(usize, VRegId, VRegId, i64, usize)> {
            let &(mul_idx, mul_op) = defs.get(&scaled)?;
            if mul_op != ArmOpcode::Mul {
                return None;
            }
            if use_count.get(&scaled).copied().unwrap_or(0) != 1 {
                return None;
            }
            let mul_inst = &block.insts[mul_idx];
            if mul_inst.operands.len() < 3 {
                return None;
            }
            let m_lhs = match &mul_inst.operands[1] {
                MachineOperand::VReg(v) => *v,
                _ => return None,
            };
            let m_rhs = match &mul_inst.operands[2] {
                MachineOperand::VReg(v) => *v,
                _ => return None,
            };
            // The constant operand of the Mul — must be defined by a
            // single-use Movz with chunk ∈ {1,2,4,8} at shift 0.
            let try_const = |const_v: VRegId, idx_v: VRegId| -> Option<(VRegId, i64, usize)> {
                let &(movz_idx, movz_op) = defs.get(&const_v)?;
                if movz_op != ArmOpcode::Movz {
                    return None;
                }
                if use_count.get(&const_v).copied().unwrap_or(0) != 1 {
                    return None;
                }
                let movz_inst = &block.insts[movz_idx];
                // Movz operands: [dest, Imm(chunk), Shift(amount)]. We
                // require shift==0 and chunk ∈ {1,2,4,8}.
                if movz_inst.operands.len() < 3 {
                    return None;
                }
                let chunk = match &movz_inst.operands[1] {
                    MachineOperand::Imm(v) => *v,
                    _ => return None,
                };
                let shift_amount = match &movz_inst.operands[2] {
                    MachineOperand::Shift(s) => *s,
                    _ => return None,
                };
                if shift_amount != 0 {
                    return None;
                }
                let lsl_shift: i64 = match chunk {
                    1 => 0,
                    2 => 1,
                    4 => 2,
                    8 => 3,
                    _ => return None,
                };
                Some((idx_v, lsl_shift, movz_idx))
            };
            try_const(m_rhs, m_lhs)
                .or_else(|| try_const(m_lhs, m_rhs))
                .map(|(idx, shift, movz_idx)| (mul_idx, base, idx, shift, movz_idx))
        };

        let resolved = try_pair(add_rhs, add_lhs).or_else(|| try_pair(add_lhs, add_rhs));
        let Some((mul_idx, base_v, idx_v, shift, movz_idx)) = resolved else {
            continue;
        };

        plans.push(FusionPlan {
            ldst_idx: i,
            add_idx,
            mul_idx,
            movz_idx,
            new_opcode,
            dest_op: inst.operands[0].clone(),
            dest_def: inst.def,
            base: base_v,
            idx: idx_v,
            shift,
        });
    }

    if plans.is_empty() {
        return;
    }

    // Apply: rewrite the load/store in place; mark the mul/add/movz
    // for removal. Iterate in reverse ldst_idx order so removals
    // before the rewrite don't shift indices.
    plans.sort_by_key(|plan| std::cmp::Reverse(plan.ldst_idx));
    let mut remove_idxs: std::collections::HashSet<usize> = plans
        .iter()
        .flat_map(|p| [p.add_idx, p.mul_idx, p.movz_idx])
        .collect();

    let block = &mut mf.blocks[mb_idx];
    for plan in &plans {
        block.insts[plan.ldst_idx] = MachineInst {
            opcode: plan.new_opcode,
            operands: vec![
                plan.dest_op.clone(),
                MachineOperand::VReg(plan.base),
                MachineOperand::VReg(plan.idx),
                MachineOperand::Imm(plan.shift),
            ],
            def: plan.dest_def,
        };
    }
    let mut idx = 0usize;
    block.insts.retain(|_| {
        let keep = !remove_idxs.remove(&idx);
        idx += 1;
        keep
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::mir::{
        ArmOpcode, MBlockId, MachineBlock, MachineFunction, MachineInst, MachineOperand, RegClass,
    };

    fn mf_with_insts(insts: Vec<MachineInst>) -> MachineFunction {
        let mut mf = MachineFunction::new("test".into());
        // Allocate enough vregs.
        for _ in 0..10 {
            mf.new_vreg(RegClass::Fp32);
        }
        let bid = MBlockId(0);
        mf.blocks = vec![MachineBlock {
            id: bid,
            label: "entry".into(),
            insts,
        }];
        mf
    }

    fn vreg(v: u32) -> MachineOperand {
        MachineOperand::VReg(VRegId(v))
    }
    fn vid(v: u32) -> VRegId {
        VRegId(v)
    }

    /// fadd(fmul(v1, v2), v3) → fmadd(v1, v2, v3)
    #[test]
    fn fmadd_f32() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulS,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fadd = MachineInst {
            opcode: ArmOpcode::FaddS,
            operands: vec![vreg(4), vreg(0), vreg(3)], // src0 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fadd]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1, "fmul should be removed");
        assert_eq!(block.insts[0].opcode, ArmOpcode::FmaddS);
        // operands: [dest, n, m, a] = [v4, v1, v2, v3]
        assert_eq!(block.insts[0].operands[1], vreg(1));
        assert_eq!(block.insts[0].operands[2], vreg(2));
        assert_eq!(block.insts[0].operands[3], vreg(3));
    }

    /// fadd(v3, fmul(v1, v2)) → fmadd(v1, v2, v3)  [commuted]
    #[test]
    fn fmadd_commuted() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulD,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fadd = MachineInst {
            opcode: ArmOpcode::FaddD,
            operands: vec![vreg(4), vreg(3), vreg(0)], // src1 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fadd]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::FmaddD);
        assert_eq!(block.insts[0].operands[3], vreg(3)); // accumulator
    }

    /// fsub(c, fmul(a,b)) → fmsub(a, b, c)  [result = c - a*b]
    #[test]
    fn fmsub_f32() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulS,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fsub = MachineInst {
            opcode: ArmOpcode::FsubS,
            operands: vec![vreg(4), vreg(3), vreg(0)], // src1 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fsub]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::FmsubS);
        assert_eq!(block.insts[0].operands[3], vreg(3)); // Sa = c
    }

    /// fsub(fmul(a,b), c) → fnmsub(a, b, c)  [result = a*b - c]
    #[test]
    fn fnmsub_f64() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulD,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        let fsub = MachineInst {
            opcode: ArmOpcode::FsubD,
            operands: vec![vreg(4), vreg(0), vreg(3)], // src0 = fmul result
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![fmul, fsub]);
        fma_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::FnmsubD);
    }

    fn vreg_gp(v: u32) -> MachineOperand {
        MachineOperand::VReg(VRegId(v))
    }

    /// add(mul(v1, v2), v3)  →  madd v4, v1, v2, v3
    #[test]
    fn integer_madd_fusion() {
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg_gp(0), vreg_gp(1), vreg_gp(2)],
            def: Some(vid(0)),
        };
        let add = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg_gp(4), vreg_gp(0), vreg_gp(3)],
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![mul, add]);
        madd_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1, "mul should be removed");
        assert_eq!(block.insts[0].opcode, ArmOpcode::Madd);
        assert_eq!(block.insts[0].operands[1], vreg_gp(1));
        assert_eq!(block.insts[0].operands[2], vreg_gp(2));
        assert_eq!(block.insts[0].operands[3], vreg_gp(3));
    }

    /// add(v3, mul(v1, v2))  →  madd v4, v1, v2, v3   (commuted)
    #[test]
    fn integer_madd_fusion_commuted() {
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg_gp(0), vreg_gp(1), vreg_gp(2)],
            def: Some(vid(0)),
        };
        let add = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg_gp(4), vreg_gp(3), vreg_gp(0)],
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![mul, add]);
        madd_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::Madd);
        assert_eq!(block.insts[0].operands[3], vreg_gp(3), "accumulator is c");
    }

    /// sub(c, mul(a, b))  →  msub a, b, c   (result = c - a*b)
    #[test]
    fn integer_msub_fusion() {
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg_gp(0), vreg_gp(1), vreg_gp(2)],
            def: Some(vid(0)),
        };
        let sub = MachineInst {
            opcode: ArmOpcode::SubReg,
            operands: vec![vreg_gp(4), vreg_gp(3), vreg_gp(0)],
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![mul, sub]);
        madd_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::Msub);
        assert_eq!(block.insts[0].operands[3], vreg_gp(3), "Xa = c");
    }

    /// sub(mul(a, b), c)  is `a*b - c`  — no integer FNMSUB form.
    /// The pass should leave it as-is.
    #[test]
    fn integer_sub_mul_first_not_fused() {
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg_gp(0), vreg_gp(1), vreg_gp(2)],
            def: Some(vid(0)),
        };
        let sub = MachineInst {
            opcode: ArmOpcode::SubReg,
            operands: vec![vreg_gp(4), vreg_gp(0), vreg_gp(3)],
            def: Some(vid(4)),
        };
        let mut mf = mf_with_insts(vec![mul, sub]);
        madd_fusion(&mut mf);
        // Both instructions should remain — there's no msub form for a*b - c.
        assert_eq!(mf.blocks[0].insts.len(), 2);
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::Mul);
        assert_eq!(mf.blocks[0].insts[1].opcode, ArmOpcode::SubReg);
    }

    /// mul result with multiple consumers must NOT fuse.
    #[test]
    fn integer_mul_used_twice_not_fused() {
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg_gp(0), vreg_gp(1), vreg_gp(2)],
            def: Some(vid(0)),
        };
        let add1 = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg_gp(4), vreg_gp(0), vreg_gp(3)],
            def: Some(vid(4)),
        };
        let add2 = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg_gp(5), vreg_gp(0), vreg_gp(3)],
            def: Some(vid(5)),
        };
        let mut mf = mf_with_insts(vec![mul, add1, add2]);
        madd_fusion(&mut mf);
        assert_eq!(mf.blocks[0].insts.len(), 3, "mul has 2 uses, can't fuse");
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::Mul);
    }

    /// fmul result used twice: must NOT be fused.
    #[test]
    fn no_fusion_if_mul_used_twice() {
        let fmul = MachineInst {
            opcode: ArmOpcode::FmulS,
            operands: vec![vreg(0), vreg(1), vreg(2)],
            def: Some(vid(0)),
        };
        // Two consumers of vreg(0):
        let fadd1 = MachineInst {
            opcode: ArmOpcode::FaddS,
            operands: vec![vreg(4), vreg(0), vreg(3)],
            def: Some(vid(4)),
        };
        let fadd2 = MachineInst {
            opcode: ArmOpcode::FaddS,
            operands: vec![vreg(5), vreg(0), vreg(3)],
            def: Some(vid(5)),
        };
        let mut mf = mf_with_insts(vec![fmul, fadd1, fadd2]);
        fma_fusion(&mut mf);
        // No fusion — fmul has 2 uses.
        assert_eq!(mf.blocks[0].insts.len(), 3);
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::FmulS);
    }

    /// LdrFpImm fed by AddReg(base, Mul(idx, Movz(8))) → LdrFpReg with lsl=3.
    #[test]
    fn scaled_addressing_real8_load() {
        let movz = MachineInst {
            opcode: ArmOpcode::Movz,
            operands: vec![vreg(0), MachineOperand::Imm(8), MachineOperand::Shift(0)],
            def: Some(vid(0)),
        };
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg(1), vreg(2), vreg(0)], // scaled = idx(2) * const(0)
            def: Some(vid(1)),
        };
        let add = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg(3), vreg(4), vreg(1)], // addr = base(4) + scaled(1)
            def: Some(vid(3)),
        };
        let ldr = MachineInst {
            opcode: ArmOpcode::LdrFpImm,
            operands: vec![vreg(5), vreg(3), MachineOperand::Imm(0)], // ldr d5, [addr, #0]
            def: Some(vid(5)),
        };
        let mut mf = mf_with_insts(vec![movz, mul, add, ldr]);
        scaled_addressing_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1, "movz/mul/add should be removed");
        assert_eq!(block.insts[0].opcode, ArmOpcode::LdrFpReg);
        assert_eq!(block.insts[0].operands[0], vreg(5)); // dest
        assert_eq!(block.insts[0].operands[1], vreg(4)); // base
        assert_eq!(block.insts[0].operands[2], vreg(2)); // idx
        assert_eq!(block.insts[0].operands[3], MachineOperand::Imm(3)); // log2(8) = 3
    }

    /// Multi-use Mul result → no fold (the mul result is consumed twice).
    #[test]
    fn scaled_addressing_no_fold_on_multi_use_mul() {
        let movz = MachineInst {
            opcode: ArmOpcode::Movz,
            operands: vec![vreg(0), MachineOperand::Imm(8), MachineOperand::Shift(0)],
            def: Some(vid(0)),
        };
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg(1), vreg(2), vreg(0)],
            def: Some(vid(1)),
        };
        let add = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg(3), vreg(4), vreg(1)],
            def: Some(vid(3)),
        };
        let ldr = MachineInst {
            opcode: ArmOpcode::LdrFpImm,
            operands: vec![vreg(5), vreg(3), MachineOperand::Imm(0)],
            def: Some(vid(5)),
        };
        // Second consumer of the mul result — extra reference to vreg(1).
        let extra = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg(6), vreg(7), vreg(1)],
            def: Some(vid(6)),
        };
        let mut mf = mf_with_insts(vec![movz, mul, add, ldr, extra]);
        scaled_addressing_fusion(&mut mf);
        // Nothing fused.
        assert_eq!(mf.blocks[0].insts.len(), 5);
        assert_eq!(mf.blocks[0].insts[3].opcode, ArmOpcode::LdrFpImm);
    }

    /// Non-power-of-two const (e.g. element size 12 from a 3-int derived
    /// type) must NOT fold — there's no `lsl` encoding for it.
    #[test]
    fn scaled_addressing_no_fold_on_non_power_of_two() {
        let movz = MachineInst {
            opcode: ArmOpcode::Movz,
            operands: vec![vreg(0), MachineOperand::Imm(12), MachineOperand::Shift(0)],
            def: Some(vid(0)),
        };
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg(1), vreg(2), vreg(0)],
            def: Some(vid(1)),
        };
        let add = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg(3), vreg(4), vreg(1)],
            def: Some(vid(3)),
        };
        let ldr = MachineInst {
            opcode: ArmOpcode::LdrImm,
            operands: vec![vreg(5), vreg(3), MachineOperand::Imm(0)],
            def: Some(vid(5)),
        };
        let mut mf = mf_with_insts(vec![movz, mul, add, ldr]);
        scaled_addressing_fusion(&mut mf);
        assert_eq!(mf.blocks[0].insts.len(), 4);
        assert_eq!(mf.blocks[0].insts[3].opcode, ArmOpcode::LdrImm);
    }

    /// StrImm gets folded too (mirror of the load path).
    #[test]
    fn scaled_addressing_int4_store() {
        let movz = MachineInst {
            opcode: ArmOpcode::Movz,
            operands: vec![vreg(0), MachineOperand::Imm(4), MachineOperand::Shift(0)],
            def: Some(vid(0)),
        };
        let mul = MachineInst {
            opcode: ArmOpcode::Mul,
            operands: vec![vreg(1), vreg(2), vreg(0)],
            def: Some(vid(1)),
        };
        let add = MachineInst {
            opcode: ArmOpcode::AddReg,
            operands: vec![vreg(3), vreg(4), vreg(1)],
            def: Some(vid(3)),
        };
        let str_inst = MachineInst {
            opcode: ArmOpcode::StrImm,
            operands: vec![vreg(5), vreg(3), MachineOperand::Imm(0)],
            def: None, // store has no def
        };
        let mut mf = mf_with_insts(vec![movz, mul, add, str_inst]);
        scaled_addressing_fusion(&mut mf);
        let block = &mf.blocks[0];
        assert_eq!(block.insts.len(), 1);
        assert_eq!(block.insts[0].opcode, ArmOpcode::StrReg);
        assert_eq!(block.insts[0].operands[3], MachineOperand::Imm(2)); // log2(4) = 2
    }
}
