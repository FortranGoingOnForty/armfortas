//! Tail call optimization (post-regalloc peephole).
//!
//! After register allocation and callee-save insertion, the machine code for
//! a call in tail position looks like:
//!
//! ```text
//!   ; arg setup (MOV xi, …)
//!   Bl _callee
//!   ; callee-save restores (LdpOffset / LdrImm / LdrFpImm) — zero or more
//!   LdpPost x29, x30, [sp], #16          ; epilogue frame restore
//!   Ret
//! ```
//!
//! We convert this to:
//!
//! ```text
//!   ; arg setup (unchanged — all spill loads happen before LdpPost)
//!   ; callee-save restores (unchanged)
//!   LdpPost x29, x30, [sp], #16
//!   B _callee                             ; tail jump (not BL)
//! ```
//!
//! Correctness argument
//! --------------------
//! * Argument registers (x0–x7, d0–d7) are loaded BEFORE the LdpPost, so
//!   they hold the correct values when the tail branch executes.
//! * Callee-saved restores (x19–x28, d8–d15) are disjoint from the
//!   argument registers; restoring them cannot clobber the args.
//! * LdpPost restores x29 (our FP) and x30 (our LR).  After it fires, LR
//!   holds our *caller's* return address.  When _callee executes its own
//!   RET, it returns to *our* caller directly — exactly what TCO requires.
//! * We only recognize this pattern when there are **no instructions between
//!   Bl and the callee-restore cluster**.  In particular, non-void calls
//!   whose return-value register survives coalesce_moves are handled because
//!   `coalesce_moves` already eliminated any `MOV x0, x0` self-moves, leaving
//!   the Bl immediately adjacent to the callee restores / LdpPost.
//! * Gate: we don't fire on non-void calls where a non-trivial result-capture
//!   sequence remains (e.g., `MOV x1, x0`) — those are left alone.

use std::collections::HashSet;
use super::mir::{ArmOpcode, MachineFunction, MachineInst, MachineOperand, PhysReg};

/// Run tail call optimization on a single machine function.
///
/// Safe to call at any optimization level; the transformation never changes
/// visible behavior and is always a code-size win (removes one instruction).
pub fn tail_call_opt(mf: &mut MachineFunction) {
    for block in &mut mf.blocks {
        let n = block.insts.len();
        if n < 2 { continue; }

        // Epilogue is always `LdpPost; Ret` at the very end.
        if block.insts[n - 1].opcode != ArmOpcode::Ret { continue; }
        if block.insts[n - 2].opcode != ArmOpcode::LdpPost { continue; }

        let ldp_idx = n - 2;

        // Walk backwards from just before LdpPost, skipping callee-save
        // restore instructions (LdpOffset, LdrImm, LdrFpImm).  Stop when
        // we find something that isn't a callee restore.
        let mut bl_candidate = ldp_idx;
        while bl_candidate > 0 {
            bl_candidate -= 1;
            match block.insts[bl_candidate].opcode {
                ArmOpcode::LdpOffset
                | ArmOpcode::LdrImm
                | ArmOpcode::LdrFpImm => {
                    // Callee-save restore — keep scanning backwards.
                }
                ArmOpcode::Bl => {
                    // Found the BL — stop here.
                    break;
                }
                _ => {
                    // Non-callee-restore, non-BL — pattern doesn't match.
                    bl_candidate = usize::MAX; // sentinel
                    break;
                }
            }
        }

        // Sentinel or scanned to index 0 without finding Bl.
        if bl_candidate == usize::MAX { continue; }
        if block.insts[bl_candidate].opcode != ArmOpcode::Bl { continue; }

        // SAFETY: reject TCO when any argument register (x0–x7) holds a value
        // derived from our frame pointer (e.g. a pointer to a stack-allocated
        // local / derived-type struct).  After the epilogue tears down our
        // frame, the callee's prologue reuses that memory; any pointer into it
        // becomes dangling.  Taint analysis: track GP registers set from
        // `sub xN, x29, #M` (alloca) and propagated through MovReg / AddReg /
        // AddImm / Mul.  If any x0–x7 is tainted, the tail call is unsafe.
        if has_frame_derived_arg(&block.insts[..bl_candidate]) {
            continue;
        }

        // Extract the call target from the Bl operand.
        let label = match block.insts[bl_candidate].operands.first() {
            Some(MachineOperand::Extern(s)) => s.clone(),
            _ => continue,  // indirect call or unexpected operand — skip
        };

        // Transform:
        //   Remove `Bl _label` at bl_candidate.
        //   Remove `Ret` (last instruction).
        //   Append `B _label` (tail branch to external symbol).
        //
        // The callee restores and LdpPost between bl_candidate and ldp_idx
        // shift down by 1 (because we removed bl_candidate), but stay in
        // the right relative order.
        block.insts.remove(bl_candidate);
        block.insts.pop(); // remove Ret (was the last instruction)
        block.insts.push(MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::Extern(label)],
            def: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Safety helpers
// ---------------------------------------------------------------------------

/// Returns true if any GP argument register (x0–x7) contains a frame-derived
/// pointer at the point of the Bl.
///
/// "Frame-derived" means the register was set — directly or transitively — from
/// a `sub xN, x29, #M` (alloca materialization).
///
/// The analysis is a forward taint propagation over both registers AND
/// FP-relative stack slots, so it correctly handles the spill/reload pattern:
///
/// ```text
/// sub  x10, x29, #4       ; x10 = frame addr (tainted)
/// str  x10, [x29, #-16]   ; slot -16 now tainted
/// ...
/// ldr  x9,  [x29, #-16]   ; x9 = frame addr (tainted via slot)
/// mov  x0,  x9            ; x0 tainted → unsafe TCO
/// ```
fn has_frame_derived_arg(insts: &[MachineInst]) -> bool {
    // GP register numbers whose current value is derived from the frame pointer.
    let mut tainted_regs: HashSet<u8> = HashSet::new();
    // FP-relative offsets whose memory contents are frame-derived pointers.
    let mut tainted_slots: HashSet<i64> = HashSet::new();

    for inst in insts {
        match inst.opcode {
            // sub xN, x29, #imm  →  xN holds a frame-relative address.
            ArmOpcode::SubImm => {
                if op_is_fp(inst, 1) {
                    if let Some(n) = op_gp(inst, 0) { tainted_regs.insert(n); }
                }
            }
            // add xN, xM, xP  (GEP: propagate taint from either source)
            ArmOpcode::AddReg => {
                if op_gp(inst, 1).is_some_and(|n| tainted_regs.contains(&n))
                    || op_gp(inst, 2).is_some_and(|n| tainted_regs.contains(&n))
                {
                    if let Some(n) = op_gp(inst, 0) { tainted_regs.insert(n); }
                }
            }
            // add xN, xM, #imm  (GEP with constant offset / address arithmetic)
            ArmOpcode::AddImm => {
                if op_is_fp(inst, 1)
                    || op_gp(inst, 1).is_some_and(|n| tainted_regs.contains(&n))
                {
                    if let Some(n) = op_gp(inst, 0) { tainted_regs.insert(n); }
                }
            }
            // mov xN, xM  (register copy — propagates taint to arg reg)
            ArmOpcode::MovReg => {
                if op_gp(inst, 1).is_some_and(|n| tainted_regs.contains(&n)) {
                    if let Some(n) = op_gp(inst, 0) { tainted_regs.insert(n); }
                }
            }
            // mul xN, xM, xP  (index computation in GEP; conservative)
            ArmOpcode::Mul => {
                if op_gp(inst, 1).is_some_and(|n| tainted_regs.contains(&n))
                    || op_gp(inst, 2).is_some_and(|n| tainted_regs.contains(&n))
                {
                    if let Some(n) = op_gp(inst, 0) { tainted_regs.insert(n); }
                }
            }
            // str xN, [x29, #off] — if xN is tainted, the slot becomes tainted.
            ArmOpcode::StrImm => {
                if op_gp(inst, 0).is_some_and(|n| tainted_regs.contains(&n))
                    && op_is_fp(inst, 1)
                {
                    if let Some(off) = op_fp_offset(inst, 2) { tainted_slots.insert(off); }
                }
            }
            // ldr xN, [x29, #off] — if the slot is tainted, xN becomes tainted.
            ArmOpcode::LdrImm => {
                if op_is_fp(inst, 1) {
                    if let Some(off) = op_fp_offset(inst, 2) {
                        if tainted_slots.contains(&off) {
                            if let Some(n) = op_gp(inst, 0) { tainted_regs.insert(n); }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Argument registers are x0–x7 (PhysReg::Gp(0)–Gp(7)).
    (0u8..8).any(|n| tainted_regs.contains(&n))
}

/// True if operand at `idx` is the frame pointer (x29 = PhysReg::Gp(29)).
#[inline]
fn op_is_fp(inst: &MachineInst, idx: usize) -> bool {
    matches!(
        inst.operands.get(idx),
        Some(MachineOperand::PhysReg(p)) if *p == PhysReg::FP
    )
}

/// GP register number (0–30) for the PhysReg::Gp operand at `idx`, or None.
#[inline]
fn op_gp(inst: &MachineInst, idx: usize) -> Option<u8> {
    match inst.operands.get(idx)? {
        MachineOperand::PhysReg(PhysReg::Gp(n)) => Some(*n),
        _ => None,
    }
}

/// Frame-pointer-relative offset for the operand at `idx`, or None.
/// Accepts both `Imm` and `FrameSlot` variants.
#[inline]
fn op_fp_offset(inst: &MachineInst, idx: usize) -> Option<i64> {
    match inst.operands.get(idx)? {
        MachineOperand::Imm(v) => Some(*v),
        MachineOperand::FrameSlot(v) => Some(*v as i64),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::mir::*;

    fn make_block(insts: Vec<MachineInst>) -> MachineBlock {
        MachineBlock { label: "test".into(), insts, id: MBlockId(0) }
    }

    fn bl(label: &str) -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![MachineOperand::Extern(label.into())],
            def: None,
        }
    }

    fn ldp_post() -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::LdpPost,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::PhysReg(PhysReg::LR),
                MachineOperand::PhysReg(PhysReg::Sp),
            ],
            def: None,
        }
    }

    fn ret() -> MachineInst {
        MachineInst { opcode: ArmOpcode::Ret, operands: vec![], def: None }
    }

    fn ldr_callee_restore() -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::LdrImm,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(19)),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(-8),
            ],
            def: Some(VRegId(19)),
        }
    }

    fn build_mf(blocks: Vec<Vec<MachineInst>>) -> MachineFunction {
        let mut mf = MachineFunction::new("test".into());
        // MachineFunction starts with one empty block; overwrite it.
        mf.blocks[0].insts = blocks[0].clone();
        for blk_insts in blocks.into_iter().skip(1) {
            let id = mf.new_block("bb");
            mf.block_mut(id).insts = blk_insts;
        }
        mf
    }

    #[test]
    fn void_tail_call_no_callee_saves() {
        // Pattern: Bl; LdpPost; Ret → LdpPost; B
        let mut mf = build_mf(vec![
            vec![bl("_foo"), ldp_post(), ret()],
        ]);
        tail_call_opt(&mut mf);
        let insts = &mf.blocks[0].insts;
        // Should now be: LdpPost, B _foo
        assert_eq!(insts.len(), 2);
        assert_eq!(insts[0].opcode, ArmOpcode::LdpPost);
        assert_eq!(insts[1].opcode, ArmOpcode::B);
        assert_eq!(
            insts[1].operands[0],
            MachineOperand::Extern("_foo".into())
        );
    }

    #[test]
    fn void_tail_call_with_callee_restore() {
        // Pattern: Bl; LdrImm(restore); LdpPost; Ret → LdrImm; LdpPost; B
        let mut mf = build_mf(vec![
            vec![bl("_bar"), ldr_callee_restore(), ldp_post(), ret()],
        ]);
        tail_call_opt(&mut mf);
        let insts = &mf.blocks[0].insts;
        assert_eq!(insts.len(), 3);
        assert_eq!(insts[0].opcode, ArmOpcode::LdrImm);
        assert_eq!(insts[1].opcode, ArmOpcode::LdpPost);
        assert_eq!(insts[2].opcode, ArmOpcode::B);
    }

    #[test]
    fn no_tco_when_non_callee_restore_between_bl_and_ldp() {
        // A non-trivial instruction (e.g., MovReg for result capture) blocks TCO.
        let mov_result = MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)), // x1, not x0
                MachineOperand::PhysReg(PhysReg::Gp(0)), // x0 (result)
            ],
            def: None,
        };
        let mut mf = build_mf(vec![
            vec![bl("_baz"), mov_result, ldp_post(), ret()],
        ]);
        tail_call_opt(&mut mf);
        let insts = &mf.blocks[0].insts;
        // No transformation — Bl should still be present.
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::Bl),
            "Bl should NOT be removed when result is captured in non-return register");
        assert!(insts.iter().any(|i| i.opcode == ArmOpcode::Ret),
            "Ret should still be present");
    }

    #[test]
    fn no_tco_when_not_ending_in_ret() {
        // Block ending in B (not Ret) — no TCO.
        let b_inst = MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::BlockRef(MBlockId(1))],
            def: None,
        };
        let mut mf = build_mf(vec![
            vec![bl("_qux"), ldp_post(), b_inst],
        ]);
        tail_call_opt(&mut mf);
        // Should still have Bl (TCO not fired because no Ret).
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::Bl));
    }
}
