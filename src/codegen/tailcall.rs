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

use super::mir::{ArmOpcode, MachineFunction, MachineInst, MachineOperand};

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
