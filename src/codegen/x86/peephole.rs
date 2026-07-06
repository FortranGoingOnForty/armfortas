//! x86 backend peephole (sprint x09), run at O2+ on vreg MIR right
//! after instruction selection — the same driver gate as
//! `arm64::peephole`, before two-address conversion and regalloc.
//!
//! Patterns:
//!
//! * **cmp-zero → test**: `MovRI v, $0` + `Cmp(lhs, v)` becomes
//!   `Test(lhs, lhs)`, deleting the MovRI when `v` has no other use.
//!   `cmp $0, r` and `test r, r` produce identical RFLAGS (SF/ZF from
//!   the value, CF=OF=0 both ways), so every consumer condition is
//!   preserved. Only the rhs-zero form rewrites: for `Cmp(v, rhs)`
//!   with v=0 the conditions read `0 <op> rhs`, which `test` does not
//!   model (0 - INT_MIN overflows; r & r never sets OF).
//! * **self-move elimination**: `MovRR v, v` is a no-op.
//!
//! Deliberately absent (recorded in `.docs/audits/x09-pass-audit.md`):
//!
//! * `xor r, r` zeroing — writes RFLAGS, so it cannot be inserted
//!   blindly near the i128 Add/Adc and cmp/jcc adjacency the backend
//!   relies on; on naive-allocator slot traffic it also pessimizes.
//!   Revisit with the linear-scan allocator (x10).
//! * `lea` folding — address arithmetic is invisible under the naive
//!   allocator's load/op/store traffic; same x10 slot.
//! * inc/dec policy: the MIR has no Inc/Dec opcodes — isel always
//!   emits Add/Sub with an immediate, which is the documented choice
//!   (inc/dec leave CF unmodified and stall on partial-flag merges).
//! * No FMA contraction: SSE2 baseline has no fused multiply-add, and
//!   contraction changes last-ulp results (see the audit's FMA policy).

use std::collections::HashMap;

use super::mir::{OpSize, X86Function, X86Inst, X86Opcode, X86Operand, X86Reg};
use crate::codegen::shared::VRegId;

/// Run all pre-regalloc peephole patterns over one function. Returns the
/// number of instructions rewritten or removed (tests assert on it; the
/// driver ignores it).
pub fn run_peephole(f: &mut X86Function) -> usize {
    let mut changed = 0;
    changed += cmp_zero_to_test(f);
    changed += drop_self_moves(f);
    changed
}

/// Run all post-regalloc peephole patterns (x10b round 2). These work on
/// physical registers and frame slots — values that only exist after
/// allocation — so they run after `apply_allocation`/`regalloc_naive`,
/// gated at O2+ by the driver.
pub fn run_peephole_post_regalloc(f: &mut X86Function) -> usize {
    let mut changed = 0;
    changed += store_to_load_forward(f);
    changed += lea_fold(f);
    changed += xor_zero(f);
    changed
}

/// Pattern 2 — lea folding (post-regalloc). An adjacent copy + add
///
///   mov %a, %d        (d != a)
///   add %b, %d        (b != d)
///
/// becomes the non-destructive three-operand
///
///   lea (%a, %b), %d
///
/// dropping the copy. Post-regalloc because a 2-vreg address can't be
/// represented pre-regalloc (the Mem operand carries physical registers
/// only) and because the copy is a real instruction only once `a` and
/// `d` are known to be distinct physical registers. lea does not write
/// flags, so the fold is gated by flags-liveness: only when the add's
/// RFLAGS result is dead. `b != d` is required for correctness — the
/// folded lea reads the pre-copy registers, and `d`'s value comes from
/// the copy being deleted. GP only.
fn lea_fold(f: &mut X86Function) -> usize {
    let mut changed = 0;
    for block in &mut f.blocks {
        let dead = flags_dead_after(&block.insts);
        let mut out: Vec<X86Inst> = Vec::with_capacity(block.insts.len());
        let mut i = 0;
        while i < block.insts.len() {
            if i + 1 < block.insts.len() {
                if let Some(lea) = try_lea_fold(&block.insts[i], &block.insts[i + 1], dead[i + 1]) {
                    out.push(lea);
                    changed += 1;
                    i += 2;
                    continue;
                }
            }
            out.push(block.insts[i].clone());
            i += 1;
        }
        block.insts = out;
    }
    changed
}

/// The `mov %a,%d` + `add %b,%d` → `lea (%a,%b),%d` match. Returns the
/// folded lea, or None if the pair does not qualify.
fn try_lea_fold(mov: &X86Inst, add: &X86Inst, add_flags_dead: bool) -> Option<X86Inst> {
    if !add_flags_dead || mov.opcode != X86Opcode::MovRR || add.opcode != X86Opcode::Add {
        return None;
    }
    // mov: def = %d, operand 0 = %a.
    let (Some(X86Operand::Reg(d)), Some(X86Operand::Reg(a))) =
        (mov.def.as_ref(), mov.operands.first())
    else {
        return None;
    };
    let (d, a) = (*d, *a);
    // add (two-address): def = %d, operands = [%d, %b].
    let Some(X86Operand::Reg(add_def)) = add.def.as_ref() else {
        return None;
    };
    let add_def = *add_def;
    let (Some(X86Operand::Reg(add_op0)), Some(X86Operand::Reg(b))) =
        (add.operands.first(), add.operands.get(1))
    else {
        return None;
    };
    if add_def != d || *add_op0 != d {
        return None;
    }
    let b = *b;
    // a != d (else the copy is a self-move and there is no win); b != d
    // (else the index would read the value the deleted copy produced).
    if a == d || b == d {
        return None;
    }
    if !d.is_gp() || !a.is_gp() || !b.is_gp() {
        return None;
    }
    Some(X86Inst {
        opcode: X86Opcode::Lea,
        size: add.size,
        operands: vec![X86Operand::Mem {
            base: Some(a),
            index: Some(b),
            scale: 1,
            disp: 0,
        }],
        def: Some(X86Operand::Reg(d)),
    })
}

/// The `disp` of a stack slot operand `[%rbp - n]` (base rbp, no index).
/// Spill/frame slots are always this shape (`mem_for_slot`); anything
/// else (an index register, a non-rbp base) is not a statically-known
/// slot and must be treated as a possible alias of every slot.
fn rbp_slot_disp(op: &X86Operand) -> Option<i64> {
    match op {
        X86Operand::Mem {
            base: Some(X86Reg::Rbp),
            index: None,
            disp,
            ..
        } => Some(*disp),
        _ => None,
    }
}

/// Registers an instruction may overwrite — used to invalidate a
/// forwarded value held in a register. Explicit `Reg` def plus the
/// implicit defs isel encodes in the opcode (Idiv/Cltd/Cqto). Call is
/// handled by the caller (clears everything).
fn clobbered_regs(inst: &X86Inst) -> Vec<X86Reg> {
    let mut regs = Vec::new();
    if let Some(X86Operand::Reg(r)) = inst.def {
        regs.push(r);
    }
    match inst.opcode {
        X86Opcode::Idiv => {
            regs.push(X86Reg::Rax);
            regs.push(X86Reg::Rdx);
        }
        X86Opcode::Cltd | X86Opcode::Cqto => regs.push(X86Reg::Rdx),
        _ => {}
    }
    regs
}

/// Pattern 3 — store-to-load forwarding (GP, post-regalloc). Within a
/// block, a `mov %rs, [slot]` followed by `mov [slot], %rd` with the slot
/// untouched and `%rs` unclobbered in between becomes `mov %rs, %rd`,
/// eliminating the reload. Block-local and conservative:
///
/// - a Call/CallReg clears all tracked slots (clobbers caller-saved and
///   may write the frame);
/// - a store to a non-slot address (unknown base/index) clears all
///   tracked slots (possible alias);
/// - a write to a known slot invalidates just that slot;
/// - any redefinition of the held value register drops that entry;
/// - only same-size forwards (the stored width must equal the loaded
///   width — a value register is only valid for the bytes that were
///   stored).
fn store_to_load_forward(f: &mut X86Function) -> usize {
    let mut changed = 0;
    for block in &mut f.blocks {
        // slot disp -> (register holding the stored value, store size)
        let mut avail: HashMap<i64, (X86Reg, OpSize)> = HashMap::new();
        for inst in &mut block.insts {
            // 1. Rewrite a forwardable load before anything else mutates
            //    state for this instruction.
            if inst.opcode == X86Opcode::MovRM {
                if let (Some(X86Operand::Reg(rd)), Some(slot_op)) =
                    (inst.def.as_ref(), inst.operands.first())
                {
                    let rd = *rd;
                    if let Some(disp) = rbp_slot_disp(slot_op) {
                        if let Some(&(rs, sz)) = avail.get(&disp) {
                            if sz == inst.size && rd.is_gp() {
                                inst.opcode = X86Opcode::MovRR;
                                inst.operands = vec![X86Operand::Reg(rs)];
                                changed += 1;
                                // `rd` now holds the value too, but it is
                                // also a fresh def — fall through to the
                                // clobber step so other slots holding `rd`
                                // are invalidated.
                            }
                        }
                    }
                }
            }

            // 2. Call clobbers caller-saved registers and may touch the
            //    frame — drop everything.
            if matches!(inst.opcode, X86Opcode::Call | X86Opcode::CallReg) {
                avail.clear();
                continue;
            }

            // 3. Invalidate entries whose held register this instruction
            //    overwrites.
            for r in clobbered_regs(inst) {
                avail.retain(|_, &mut (held, _)| held != r);
            }

            // 4. Memory writes. A GP store (MovMR: operands [reg, mem])
            //    or any def to memory updates/invalidates a slot; a write
            //    to a non-slot address may alias any slot.
            let mut handled_store = false;
            if inst.opcode == X86Opcode::MovMR {
                if let (Some(X86Operand::Reg(rs)), Some(mem)) =
                    (inst.operands.first(), inst.operands.get(1))
                {
                    if let Some(disp) = rbp_slot_disp(mem) {
                        avail.insert(disp, (*rs, inst.size));
                        handled_store = true;
                    } else {
                        // store to an unknown address: alias-clear.
                        avail.clear();
                        handled_store = true;
                    }
                }
            }
            if !handled_store {
                // Any other write to memory (MovMI, FP stores with a Mem
                // def, etc.): invalidate the named slot, or clear all if
                // the destination address is not a known slot.
                if let Some(def) = &inst.def {
                    if matches!(def, X86Operand::Mem { .. }) {
                        match rbp_slot_disp(def) {
                            Some(disp) => {
                                avail.remove(&disp);
                            }
                            None => avail.clear(),
                        }
                    }
                }
                // MovMI store: mem <- imm, def None, operands [imm, mem].
                if inst.opcode == X86Opcode::MovMI {
                    if let Some(mem) = inst.operands.get(1) {
                        match rbp_slot_disp(mem) {
                            Some(disp) => {
                                avail.remove(&disp);
                            }
                            None => avail.clear(),
                        }
                    }
                }
            }
        }
    }
    changed
}

/// How an opcode affects RFLAGS. The MIR has no flags register class
/// (mir.rs: "RFLAGS is implicit"), so flags-liveness is reconstructed
/// from the opcode here.
#[derive(Clone, Copy, PartialEq)]
enum FlagEffect {
    /// Fully defines RFLAGS without reading it.
    Writer,
    /// Reads RFLAGS.
    Reader,
    /// Reads and writes (the i128 pair ops).
    ReadWrite,
    /// No effect on RFLAGS.
    Transparent,
}

fn flag_effect(op: X86Opcode) -> FlagEffect {
    use X86Opcode::*;
    match op {
        // Idiv leaves flags undefined — treat as a clobbering writer.
        Add | Sub | Imul | And | Or | Xor | Neg | Shl | Shr | Sar | Bsr | Bsf | Cmp | Test
        | Idiv => FlagEffect::Writer,
        Jcc | Setcc => FlagEffect::Reader,
        Adc | Sbb => FlagEffect::ReadWrite,
        // Everything else (Mov*, Lea, Cltd/Cqto, Not, Push/Pop, Call,
        // Ret, all SSE) leaves RFLAGS untouched. `not` notably does NOT
        // modify flags on x86.
        _ => FlagEffect::Transparent,
    }
}

/// For each instruction in a block, is RFLAGS dead immediately *after*
/// it executes? A pattern that introduces a flag write at position i is
/// safe iff `dead[i]`. The block-local backward scan is exact within the
/// block; the only approximation is the block's flags-live-out:
///
/// - A block ending in `Ret` has live-out = false: the SysV ABI does not
///   preserve arithmetic flags across a function boundary, so no
///   successor reads them. This captures the common "return 0" idiom.
/// - Every other block (Jmp/Jcc/fallthrough terminators) uses the
///   conservative live-out = true. A false "live" only forgoes an
///   optimization; an under-approximated live-out would miscompile, so
///   the conservative direction is the safe one and no successor-map
///   dataflow (with its fallthrough-edge hazards) is attempted.
fn flags_dead_after(insts: &[X86Inst]) -> Vec<bool> {
    let n = insts.len();
    let mut dead = vec![false; n];
    let ends_in_ret = insts.last().is_some_and(|i| i.opcode == X86Opcode::Ret);
    // `live` = RFLAGS liveness at the point after the current instruction
    // (= live-in of the next).
    let mut live = !ends_in_ret;
    for i in (0..n).rev() {
        dead[i] = !live;
        match flag_effect(insts[i].opcode) {
            FlagEffect::Reader | FlagEffect::ReadWrite => live = true,
            FlagEffect::Writer => live = false,
            FlagEffect::Transparent => {}
        }
    }
    dead
}

/// Pattern 1 — xor-zeroing. `mov $0, %r` → `xor %r, %r` (GP only), when
/// RFLAGS is dead right after the materialization. `xorl %r32, %r32`
/// zeroes the full 64-bit register (implicit zero-extension), encodes in
/// 2-3 bytes vs 5-7 for `mov $0`, and breaks the false dependency. The
/// flags-dead guard protects the i128 Adc/Sbb carry chains (Adc reads
/// flags, so a zero between Add and Adc reports flags-live) and cmp/jcc
/// adjacency.
fn xor_zero(f: &mut X86Function) -> usize {
    let mut changed = 0;
    for block in &mut f.blocks {
        let dead = flags_dead_after(&block.insts);
        for (i, inst) in block.insts.iter_mut().enumerate() {
            if inst.opcode != X86Opcode::MovRI {
                continue;
            }
            if !matches!(inst.operands.first(), Some(X86Operand::Imm(0))) {
                continue;
            }
            let Some(X86Operand::Reg(r)) = inst.def else {
                continue;
            };
            if !r.is_gp() {
                continue;
            }
            if !dead[i] {
                continue;
            }
            inst.opcode = X86Opcode::Xor;
            inst.size = OpSize::L;
            inst.operands = vec![X86Operand::Reg(r), X86Operand::Reg(r)];
            changed += 1;
        }
    }
    changed
}

/// Count uses of every vreg in operand position (defs excluded).
fn use_counts(f: &X86Function) -> HashMap<VRegId, usize> {
    let mut counts: HashMap<VRegId, usize> = HashMap::new();
    for block in &f.blocks {
        for inst in &block.insts {
            for op in &inst.operands {
                if let X86Operand::VReg(v) = op {
                    *counts.entry(v.id).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

fn cmp_zero_to_test(f: &mut X86Function) -> usize {
    let uses = use_counts(f);

    // The const may be materialized far from the Cmp (SSA constants
    // select once, often in the entry block), so block-local tracking
    // misses most sites. A vreg is a usable zero when its ONLY def in
    // the whole function is `MovRI v, $0` — multi-def vregs (block
    // param copies) are skipped.
    let mut def_counts: HashMap<VRegId, usize> = HashMap::new();
    let mut zero_site: HashMap<VRegId, (usize, usize)> = HashMap::new();
    for (bi, block) in f.blocks.iter().enumerate() {
        for (ii, inst) in block.insts.iter().enumerate() {
            if let Some(X86Operand::VReg(d)) = &inst.def {
                *def_counts.entry(d.id).or_insert(0) += 1;
                if inst.opcode == X86Opcode::MovRI
                    && matches!(inst.operands.first(), Some(X86Operand::Imm(0)))
                {
                    zero_site.insert(d.id, (bi, ii));
                }
            }
        }
    }
    let is_zero = |id: VRegId| def_counts.get(&id) == Some(&1) && zero_site.contains_key(&id);

    let mut changed = 0;
    let mut rewritten_uses: HashMap<VRegId, usize> = HashMap::new();
    for block in &mut f.blocks {
        for inst in &mut block.insts {
            if inst.opcode != X86Opcode::Cmp {
                continue;
            }
            let (Some(X86Operand::VReg(lhs)), Some(X86Operand::VReg(rhs))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            let (lhs, rhs) = (*lhs, *rhs);
            if !is_zero(rhs.id) {
                continue;
            }
            inst.opcode = X86Opcode::Test;
            inst.operands = vec![X86Operand::VReg(lhs), X86Operand::VReg(lhs)];
            *rewritten_uses.entry(rhs.id).or_insert(0) += 1;
            changed += 1;
        }
    }

    // Delete a zero's MovRI only when every use was rewritten away.
    let mut dead: Vec<(usize, usize)> = zero_site
        .iter()
        .filter(|(id, _)| rewritten_uses.get(id) == uses.get(id) && rewritten_uses.contains_key(id))
        .map(|(_, site)| *site)
        .collect();
    dead.sort_unstable();
    for &(bi, ii) in dead.iter().rev() {
        f.blocks[bi].insts.remove(ii);
    }
    changed
}

fn drop_self_moves(f: &mut X86Function) -> usize {
    let mut changed = 0;
    for block in &mut f.blocks {
        let before = block.insts.len();
        block.insts.retain(|inst| {
            if inst.opcode != X86Opcode::MovRR {
                return true;
            }
            match (&inst.def, inst.operands.first()) {
                (Some(X86Operand::VReg(d)), Some(X86Operand::VReg(s))) => d.id != s.id,
                (Some(X86Operand::Reg(d)), Some(X86Operand::Reg(s))) => d != s,
                _ => true,
            }
        });
        changed += before - block.insts.len();
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::shared::MBlockId;
    use crate::codegen::shared::VRegId;
    use crate::codegen::x86::mir::{OpSize, X86Block, X86Cond, X86Inst, X86RegClass, X86VReg};

    fn vreg(id: u32) -> X86VReg {
        X86VReg {
            id: VRegId(id),
            class: X86RegClass::Gp64,
        }
    }

    fn func_with(insts: Vec<X86Inst>) -> X86Function {
        let mut f = X86Function::new("t".into());
        f.blocks = vec![X86Block {
            id: MBlockId(0),
            insts,
        }];
        f
    }

    fn movri(dst: u32, imm: i64) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::MovRI,
            size: OpSize::Q,
            operands: vec![X86Operand::Imm(imm)],
            def: Some(X86Operand::VReg(vreg(dst))),
        }
    }

    fn cmp(a: u32, b: u32) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Cmp,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(vreg(a)), X86Operand::VReg(vreg(b))],
            def: None,
        }
    }

    fn setcc(cond: X86Cond, dst: u32) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Setcc,
            size: OpSize::B,
            operands: vec![X86Operand::Cond(cond)],
            def: Some(X86Operand::VReg(vreg(dst))),
        }
    }

    #[test]
    fn cmp_with_zero_rhs_becomes_test_and_movri_dies() {
        let mut f = func_with(vec![movri(1, 0), cmp(0, 1), setcc(X86Cond::E, 2)]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 1);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 2, "MovRI should be deleted: {:?}", insts);
        assert_eq!(insts[0].opcode, X86Opcode::Test);
        assert_eq!(
            insts[0].operands,
            vec![X86Operand::VReg(vreg(0)), X86Operand::VReg(vreg(0))]
        );
    }

    #[test]
    fn cmp_with_zero_lhs_is_left_alone() {
        // 0 <op> rhs is not test-representable (see module doc).
        let mut f = func_with(vec![movri(1, 0), cmp(1, 0), setcc(X86Cond::L, 2)]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 0);
        assert_eq!(f.blocks[0].insts[1].opcode, X86Opcode::Cmp);
    }

    #[test]
    fn multi_use_zero_rewrites_all_and_movri_dies() {
        let mut f = func_with(vec![
            movri(1, 0),
            cmp(0, 1),
            cmp(2, 1),
            setcc(X86Cond::E, 3),
        ]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 2, "both cmps rewrite");
        let insts = &f.blocks[0].insts;
        assert_eq!(
            insts.len(),
            3,
            "all uses rewritten, MovRI dies: {:?}",
            insts
        );
        assert_eq!(insts[0].opcode, X86Opcode::Test);
        assert_eq!(insts[1].opcode, X86Opcode::Test);
    }

    #[test]
    fn zero_with_surviving_non_cmp_use_keeps_the_movri() {
        // The zero also feeds an Add — only the Cmp use rewrites, so
        // the materialization must stay.
        let add = X86Inst {
            opcode: X86Opcode::Add,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(vreg(2)), X86Operand::VReg(vreg(1))],
            def: Some(X86Operand::VReg(vreg(2))),
        };
        let mut f = func_with(vec![movri(1, 0), cmp(0, 1), add]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 1);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 3, "MovRI must survive: {:?}", insts);
        assert_eq!(insts[0].opcode, X86Opcode::MovRI);
        assert_eq!(insts[1].opcode, X86Opcode::Test);
    }

    #[test]
    fn redefined_vreg_is_not_treated_as_zero() {
        let mut f = func_with(vec![movri(1, 0), movri(1, 7), cmp(0, 1)]);
        let changed = run_peephole(&mut f);
        assert_eq!(changed, 0);
        assert_eq!(f.blocks[0].insts[2].opcode, X86Opcode::Cmp);
    }

    #[test]
    fn nonzero_immediate_is_not_rewritten() {
        let mut f = func_with(vec![movri(1, 5), cmp(0, 1)]);
        assert_eq!(run_peephole(&mut f), 0);
    }

    #[test]
    fn self_move_dropped() {
        let mut f = func_with(vec![X86Inst {
            opcode: X86Opcode::MovRR,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(vreg(3))],
            def: Some(X86Operand::VReg(vreg(3))),
        }]);
        assert_eq!(run_peephole(&mut f), 1);
        assert!(f.blocks[0].insts.is_empty());
    }

    // ---- x10b post-regalloc: xor-zeroing ----

    fn movri_phys(dst: X86Reg, imm: i64) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::MovRI,
            size: OpSize::Q,
            operands: vec![X86Operand::Imm(imm)],
            def: Some(X86Operand::Reg(dst)),
        }
    }

    fn cmp_phys(a: X86Reg, b: X86Reg) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Cmp,
            size: OpSize::Q,
            operands: vec![X86Operand::Reg(a), X86Operand::Reg(b)],
            def: None,
        }
    }

    fn jcc(cond: X86Cond) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Jcc,
            size: OpSize::Q,
            operands: vec![X86Operand::Cond(cond), X86Operand::BlockRef(MBlockId(0))],
            def: None,
        }
    }

    fn add_phys(d: X86Reg, s: X86Reg) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Add,
            size: OpSize::Q,
            operands: vec![X86Operand::Reg(d), X86Operand::Reg(s)],
            def: Some(X86Operand::Reg(d)),
        }
    }

    fn ret() -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Ret,
            size: OpSize::Q,
            operands: vec![],
            def: None,
        }
    }

    #[test]
    fn xor_zero_fires_when_flags_dead() {
        // mov $0,%rax; ret — flags dead at the Ret boundary → xorl.
        let mut f = func_with(vec![movri_phys(X86Reg::Rax, 0), ret()]);
        assert_eq!(run_peephole_post_regalloc(&mut f), 1);
        let inst = &f.blocks[0].insts[0];
        assert_eq!(inst.opcode, X86Opcode::Xor);
        assert_eq!(inst.size, OpSize::L);
        assert_eq!(
            inst.operands,
            vec![X86Operand::Reg(X86Reg::Rax), X86Operand::Reg(X86Reg::Rax)]
        );
        assert_eq!(inst.def, Some(X86Operand::Reg(X86Reg::Rax)));
    }

    #[test]
    fn xor_zero_blocked_at_block_tail_without_ret() {
        // A zero as the last instruction of a non-Ret block keeps the
        // conservative live-out, so it is not converted.
        let mut f = func_with(vec![movri_phys(X86Reg::Rax, 0)]);
        assert_eq!(run_peephole_post_regalloc(&mut f), 0);
        assert_eq!(f.blocks[0].insts[0].opcode, X86Opcode::MovRI);
    }

    #[test]
    fn xor_zero_blocked_when_flags_live_for_jcc() {
        // cmp; mov $0; jcc — the zero sits between a flags producer and
        // its consumer, so converting to xor would clobber the carry.
        let mut f = func_with(vec![
            cmp_phys(X86Reg::Rbx, X86Reg::Rcx),
            movri_phys(X86Reg::Rax, 0),
            jcc(X86Cond::E),
        ]);
        assert_eq!(run_peephole_post_regalloc(&mut f), 0);
        assert_eq!(f.blocks[0].insts[1].opcode, X86Opcode::MovRI);
    }

    #[test]
    fn xor_zero_blocked_between_add_and_adc() {
        // i128 carry chain: add (low); mov $0; adc (high). Adc reads
        // flags, so the zero must not become xor.
        let adc = X86Inst {
            opcode: X86Opcode::Adc,
            size: OpSize::Q,
            operands: vec![X86Operand::Reg(X86Reg::R8), X86Operand::Reg(X86Reg::R9)],
            def: Some(X86Operand::Reg(X86Reg::R8)),
        };
        let mut f = func_with(vec![
            add_phys(X86Reg::Rsi, X86Reg::Rdi),
            movri_phys(X86Reg::Rax, 0),
            adc,
        ]);
        assert_eq!(run_peephole_post_regalloc(&mut f), 0);
    }

    #[test]
    fn xor_zero_fires_when_writer_shadows_later_reader() {
        // mov $0; cmp; jcc — the cmp redefines flags before the jcc, so
        // the zero's position has dead flags and converts.
        let mut f = func_with(vec![
            movri_phys(X86Reg::Rax, 0),
            cmp_phys(X86Reg::Rbx, X86Reg::Rcx),
            jcc(X86Cond::E),
        ]);
        assert_eq!(run_peephole_post_regalloc(&mut f), 1);
        assert_eq!(f.blocks[0].insts[0].opcode, X86Opcode::Xor);
    }

    // ---- x10b post-regalloc: store-to-load forwarding ----

    fn slot(disp: i64) -> X86Operand {
        X86Operand::Mem {
            base: Some(X86Reg::Rbp),
            index: None,
            scale: 1,
            disp,
        }
    }

    fn store_slot(rs: X86Reg, disp: i64, size: OpSize) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::MovMR,
            size,
            operands: vec![X86Operand::Reg(rs), slot(disp)],
            def: None,
        }
    }

    fn load_slot(rd: X86Reg, disp: i64, size: OpSize) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::MovRM,
            size,
            operands: vec![slot(disp)],
            def: Some(X86Operand::Reg(rd)),
        }
    }

    fn call() -> X86Inst {
        X86Inst {
            opcode: X86Opcode::Call,
            size: OpSize::Q,
            operands: vec![],
            def: None,
        }
    }

    #[test]
    fn store_load_forward_basic() {
        // mov %rsi, -8(%rbp); ...; mov -8(%rbp), %rdi  =>  mov %rsi, %rdi
        let mut f = func_with(vec![
            store_slot(X86Reg::Rsi, -8, OpSize::Q),
            add_phys(X86Reg::Rbx, X86Reg::Rcx),
            load_slot(X86Reg::Rdi, -8, OpSize::Q),
        ]);
        assert_eq!(store_to_load_forward(&mut f), 1);
        let load = &f.blocks[0].insts[2];
        assert_eq!(load.opcode, X86Opcode::MovRR);
        assert_eq!(load.operands, vec![X86Operand::Reg(X86Reg::Rsi)]);
        assert_eq!(load.def, Some(X86Operand::Reg(X86Reg::Rdi)));
    }

    #[test]
    fn store_load_blocked_by_clobber_of_value_reg() {
        // The stored register rsi is overwritten before the load.
        let mut f = func_with(vec![
            store_slot(X86Reg::Rsi, -8, OpSize::Q),
            X86Inst {
                opcode: X86Opcode::MovRR,
                size: OpSize::Q,
                operands: vec![X86Operand::Reg(X86Reg::Rcx)],
                def: Some(X86Operand::Reg(X86Reg::Rsi)),
            },
            load_slot(X86Reg::Rdi, -8, OpSize::Q),
        ]);
        assert_eq!(store_to_load_forward(&mut f), 0);
        assert_eq!(f.blocks[0].insts[2].opcode, X86Opcode::MovRM);
    }

    #[test]
    fn store_load_blocked_by_call() {
        let mut f = func_with(vec![
            store_slot(X86Reg::Rsi, -8, OpSize::Q),
            call(),
            load_slot(X86Reg::Rdi, -8, OpSize::Q),
        ]);
        assert_eq!(store_to_load_forward(&mut f), 0);
    }

    #[test]
    fn store_load_blocked_by_size_mismatch() {
        // 4-byte store, 8-byte load: the value reg is only valid for the
        // stored width.
        let mut f = func_with(vec![
            store_slot(X86Reg::Rsi, -8, OpSize::L),
            load_slot(X86Reg::Rdi, -8, OpSize::Q),
        ]);
        assert_eq!(store_to_load_forward(&mut f), 0);
    }

    #[test]
    fn store_load_forwards_most_recent_store() {
        // A second store to the same slot replaces the source.
        let mut f = func_with(vec![
            store_slot(X86Reg::Rsi, -8, OpSize::Q),
            store_slot(X86Reg::Rdx, -8, OpSize::Q),
            load_slot(X86Reg::Rdi, -8, OpSize::Q),
        ]);
        assert_eq!(store_to_load_forward(&mut f), 1);
        assert_eq!(
            f.blocks[0].insts[2].operands,
            vec![X86Operand::Reg(X86Reg::Rdx)]
        );
    }

    // ---- x10b post-regalloc: lea folding ----

    fn movrr(d: X86Reg, s: X86Reg) -> X86Inst {
        X86Inst {
            opcode: X86Opcode::MovRR,
            size: OpSize::Q,
            operands: vec![X86Operand::Reg(s)],
            def: Some(X86Operand::Reg(d)),
        }
    }

    #[test]
    fn lea_fold_basic() {
        // mov %rsi,%rax; add %rcx,%rax; ret  =>  lea (%rsi,%rcx),%rax
        let mut f = func_with(vec![
            movrr(X86Reg::Rax, X86Reg::Rsi),
            add_phys(X86Reg::Rax, X86Reg::Rcx),
            ret(),
        ]);
        assert_eq!(lea_fold(&mut f), 1);
        let insts = &f.blocks[0].insts;
        assert_eq!(insts.len(), 2, "mov folded away: {:?}", insts);
        assert_eq!(insts[0].opcode, X86Opcode::Lea);
        assert_eq!(
            insts[0].operands,
            vec![X86Operand::Mem {
                base: Some(X86Reg::Rsi),
                index: Some(X86Reg::Rcx),
                scale: 1,
                disp: 0,
            }]
        );
        assert_eq!(insts[0].def, Some(X86Operand::Reg(X86Reg::Rax)));
    }

    #[test]
    fn lea_fold_blocked_when_flags_live() {
        // mov; add; jcc — the add's flags feed the branch, so the flag
        // write cannot be dropped.
        let mut f = func_with(vec![
            movrr(X86Reg::Rax, X86Reg::Rsi),
            add_phys(X86Reg::Rax, X86Reg::Rcx),
            jcc(X86Cond::E),
        ]);
        assert_eq!(lea_fold(&mut f), 0);
        assert_eq!(f.blocks[0].insts[0].opcode, X86Opcode::MovRR);
    }

    #[test]
    fn lea_fold_blocked_when_index_is_dest() {
        // mov %rsi,%rax; add %rax,%rax — index == dest, folding would
        // read the value the deleted copy produced.
        let mut f = func_with(vec![
            movrr(X86Reg::Rax, X86Reg::Rsi),
            add_phys(X86Reg::Rax, X86Reg::Rax),
            ret(),
        ]);
        assert_eq!(lea_fold(&mut f), 0);
    }

    #[test]
    fn lea_fold_skips_self_move_source() {
        // mov %rax,%rax; add %rcx,%rax — a == d, no copy to save.
        let mut f = func_with(vec![
            movrr(X86Reg::Rax, X86Reg::Rax),
            add_phys(X86Reg::Rax, X86Reg::Rcx),
            ret(),
        ]);
        assert_eq!(lea_fold(&mut f), 0);
    }

    #[test]
    fn store_load_blocked_by_idiv_clobber() {
        // Idiv clobbers rax/rdx implicitly; a value held in rax is stale.
        let idiv = X86Inst {
            opcode: X86Opcode::Idiv,
            size: OpSize::Q,
            operands: vec![X86Operand::Reg(X86Reg::Rcx)],
            def: None,
        };
        let mut f = func_with(vec![
            store_slot(X86Reg::Rax, -8, OpSize::Q),
            idiv,
            load_slot(X86Reg::Rdi, -8, OpSize::Q),
        ]);
        assert_eq!(store_to_load_forward(&mut f), 0);
    }

    #[test]
    fn xor_zero_ignores_nonzero_and_vreg_and_xmm() {
        // Non-zero imm, an unallocated vreg def, and an xmm reg are all
        // left alone.
        let mut f = func_with(vec![
            movri_phys(X86Reg::Rax, 7),
            movri(5, 0),
            movri_phys(X86Reg::Xmm0, 0),
        ]);
        assert_eq!(run_peephole_post_regalloc(&mut f), 0);
    }
}
