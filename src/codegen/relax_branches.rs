//! ARM64 conditional-branch range relaxation.
//!
//! `B.cond` carries a 19-bit signed PC-relative immediate, so its
//! target must lie within ±1MB of the branch. Single-function bodies
//! that exceed that span (notably `stdlib_slaruv`, ~363KB of compiled
//! assembly with cross-body conditional jumps) trip the assembler's
//! "fixup value out of range" error.
//!
//! This pass runs after register allocation and tail-call optimization,
//! before assembly emission. It walks the function's blocks in linear
//! order, computes an approximate byte offset for every block label,
//! and for each `B.cond far_label` whose distance exceeds the
//! conditional-branch encoding window expands it to the trampoline
//! pattern:
//!
//! ```text
//! Before:                     After (when target is too far):
//!   b.cond  far_label             b.{!cond}  skip       ; one branch over
//!                                 b          far_label  ; ±128MB unconditional
//!                               skip:                   ; falls through
//! ```
//!
//! Inserting an extra block + branch shifts subsequent block offsets,
//! which can push other previously-in-range branches out of range,
//! so we iterate the pass until no further insertions are needed
//! (capped at four iterations — non-convergence triggers a panic that
//! survives release builds and produces a clear ICE for diagnostics).
//!
//! ARM64 unconditional `B` and `BL` carry a 26-bit immediate (±128MB
//! range), so the trampoline's unconditional `B` itself never needs
//! relaxation — no realistic single Fortran source produces a
//! function body larger than 128MB.

use super::emit::emit_inst_text;
use super::mir::{
    ArmCond, ArmOpcode, MBlockId, MachineBlock, MachineFunction, MachineInst, MachineOperand,
};

/// Maximum signed offset (in bytes) reachable by a `B.cond` and by
/// `cbz/cbnz`. The 19-bit signed immediate is scaled by 4, giving
/// ±(2^20) bytes. Subtract a safety margin so we never sit right on
/// the edge: a downstream peephole insertion or fall-through fixup
/// that nudges the offset by a single instruction shouldn't push us
/// over.
const COND_BRANCH_LIMIT: i64 = (1 << 20) - 64;

/// Maximum signed offset reachable by `tbz`/`tbnz`. The 14-bit
/// signed immediate is scaled by 4, giving ±(2^15) bytes — much
/// tighter than the cond-branch / cbz limit. Same safety margin.
const TBZ_BRANCH_LIMIT: i64 = (1 << 15) - 64;

/// Iteration cap. In practice 1–2 passes suffice; if convergence
/// genuinely doesn't happen, something pathological is going on and
/// we want a loud failure rather than a silent infinite loop.
const MAX_ITERATIONS: u32 = 4;

/// Run branch relaxation on a machine function. Idempotent: when no
/// `B.cond` overflows, the function is returned unchanged.
pub fn relax_branches(mf: &mut MachineFunction) {
    for _ in 0..MAX_ITERATIONS {
        if !relax_once(mf) {
            return;
        }
    }
    // Convergence failure — extremely unlikely. Better to ICE here
    // than emit an assembly file the assembler will reject.
    panic!(
        "branch relaxation did not converge for function {} after {} iterations",
        mf.name, MAX_ITERATIONS
    );
}

/// Single relaxation pass. Returns `true` when at least one branch
/// was expanded (caller should iterate); `false` when every `B.cond`
/// is in range.
fn relax_once(mf: &mut MachineFunction) -> bool {
    // Compute byte offsets for every block label using the actual
    // emit-time instruction count. Several MIR opcodes lower to
    // multiple ARM64 instructions (AdrpLdr / AdrpAdd, prologue +
    // stack-probe sequences, large-immediate movz/movk chains, etc.),
    // so a flat 4-bytes-per-MachineInst estimate would systematically
    // under-shoot the real offsets in functions like stdlib_slaruv
    // and miss far branches that need relaxation.
    let block_offsets = compute_block_offsets(mf);

    // Collect overflow sites before mutating anything. We also record
    // the per-instruction prefix offset within each block so we can
    // skip past wide-emit insts that precede the conditional branch.
    let mut overflows: Vec<OverflowSite> = Vec::new();
    for block in &mf.blocks {
        let block_offset = block_offsets[&block.id];
        let mut running = 0i64;
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            if let Some((target, limit, kind)) = relaxable_cond_branch(inst) {
                if let Some(&target_offset) = block_offsets.get(&target) {
                    let branch_offset = block_offset + running;
                    let delta = target_offset - branch_offset;
                    if delta.abs() > limit {
                        overflows.push(OverflowSite {
                            block_id: block.id,
                            inst_idx,
                            target,
                            kind,
                        });
                    }
                }
            }
            running += inst_emit_bytes(inst, mf) as i64;
        }
    }

    if overflows.is_empty() {
        return false;
    }

    // Expand each overflow. Sort in reverse order of (block_id index,
    // inst_idx) so earlier expansions don't invalidate later inst_idx
    // values — block-vec insertions happen at the back of the affected
    // block group.
    overflows.sort_by(|a, b| {
        let a_pos = block_position(mf, a.block_id);
        let b_pos = block_position(mf, b.block_id);
        b_pos.cmp(&a_pos).then(b.inst_idx.cmp(&a.inst_idx))
    });

    for site in overflows {
        expand_to_trampoline(mf, site);
    }

    true
}

#[derive(Clone)]
struct OverflowSite {
    block_id: MBlockId,
    inst_idx: usize,
    target: MBlockId,
    kind: RelaxKind,
}

#[derive(Clone)]
enum RelaxKind {
    /// `b.cond far` → `b.{!cond} skip; b far`
    BCond { cond: ArmCond },
    /// `cbz/cbnz reg far` → `cbnz/cbz reg skip; b far`
    Cbz { invert_to: ArmOpcode, reg: MachineOperand },
    /// `tbz/tbnz reg, #bit far` → `tbnz/tbz reg, #bit skip; b far`
    Tbz {
        invert_to: ArmOpcode,
        reg: MachineOperand,
        bit: i64,
    },
}

/// Inspect a machine instruction; if it is a conditional branch that
/// might need relaxation, return the target, the per-opcode range
/// limit, and a kind descriptor capturing what the inverted form
/// looks like. Returns `None` for non-branch instructions or branches
/// whose targets are unrecoverable from operands.
fn relaxable_cond_branch(inst: &MachineInst) -> Option<(MBlockId, i64, RelaxKind)> {
    match inst.opcode {
        ArmOpcode::BCond => {
            let target = bcond_target(inst)?;
            let cond = bcond_cond(inst)?;
            Some((target, COND_BRANCH_LIMIT, RelaxKind::BCond { cond }))
        }
        ArmOpcode::Cbz | ArmOpcode::Cbnz => {
            // Operands: [reg, BlockRef]
            let target = match inst.operands.get(1)? {
                MachineOperand::BlockRef(id) => *id,
                _ => return None,
            };
            let reg = inst.operands.first()?.clone();
            let invert_to = match inst.opcode {
                ArmOpcode::Cbz => ArmOpcode::Cbnz,
                _ => ArmOpcode::Cbz,
            };
            Some((target, COND_BRANCH_LIMIT, RelaxKind::Cbz { invert_to, reg }))
        }
        ArmOpcode::Tbz | ArmOpcode::Tbnz => {
            // Operands: [reg, Imm(bit), BlockRef]
            let bit = match inst.operands.get(1)? {
                MachineOperand::Imm(v) => *v,
                _ => return None,
            };
            let target = match inst.operands.get(2)? {
                MachineOperand::BlockRef(id) => *id,
                _ => return None,
            };
            let reg = inst.operands.first()?.clone();
            let invert_to = match inst.opcode {
                ArmOpcode::Tbz => ArmOpcode::Tbnz,
                _ => ArmOpcode::Tbz,
            };
            Some((
                target,
                TBZ_BRANCH_LIMIT,
                RelaxKind::Tbz {
                    invert_to,
                    reg,
                    bit,
                },
            ))
        }
        _ => None,
    }
}

/// Compute byte offsets for every block label, summing the actual
/// emit-time instruction byte count for every MachineInst in linear
/// order. Each emitted ARM64 instruction is 4 bytes; we count the
/// `\n` separators in the emitted text to figure out how many real
/// instructions an opcode produces (most are 1, but pseudo-ops and
/// large-immediate forms emit 2+).
fn compute_block_offsets(mf: &MachineFunction) -> std::collections::HashMap<MBlockId, i64> {
    let mut offsets = std::collections::HashMap::with_capacity(mf.blocks.len());
    let mut running: i64 = 0;
    for block in &mf.blocks {
        offsets.insert(block.id, running);
        for inst in &block.insts {
            running += inst_emit_bytes(inst, mf) as i64;
        }
    }
    offsets
}

/// Number of bytes a single MachineInst emits at assembly time.
/// Counted from the rendered text — `emit_inst_text` already produces
/// exactly the lines `emit_function` would, so newline-count + 1
/// matches the real instruction count without re-deriving each
/// opcode's expansion rules here.
fn inst_emit_bytes(inst: &MachineInst, mf: &MachineFunction) -> u32 {
    let text = emit_inst_text(inst, mf);
    let lines = text.matches('\n').count() as u32 + 1;
    4 * lines
}

fn block_position(mf: &MachineFunction, id: MBlockId) -> usize {
    mf.blocks
        .iter()
        .position(|b| b.id == id)
        .expect("block id present in block list")
}

fn bcond_target(inst: &MachineInst) -> Option<MBlockId> {
    inst.operands.iter().find_map(|op| match op {
        MachineOperand::BlockRef(id) => Some(*id),
        _ => None,
    })
}

fn bcond_cond(inst: &MachineInst) -> Option<ArmCond> {
    inst.operands.iter().find_map(|op| match op {
        MachineOperand::Cond(c) => Some(*c),
        _ => None,
    })
}

/// Replace an out-of-range conditional branch at `(block_id, inst_idx)`
/// with the trampoline pattern. Allocates a fresh skip block and
/// inserts it right after the original block in `mf.blocks`. The
/// inverted-condition branch jumps over the unconditional `b`, which
/// has the full ±128MB reach of the unconditional encoding — so the
/// trampoline itself never needs further relaxation regardless of
/// which conditional opcode we started from.
fn expand_to_trampoline(mf: &mut MachineFunction, site: OverflowSite) {
    let OverflowSite {
        block_id,
        inst_idx,
        target: far_target,
        kind,
    } = site;

    // Allocate a fresh skip-block id without disturbing any other
    // bookkeeping. We can't call `mf.new_block` directly because it
    // pushes onto the back of the vec; we want the skip block
    // physically adjacent to the original block (so its label
    // fall-through reaches the original successor).
    let skip_id = MBlockId(mf.next_block_id());
    let skip_label = format!("{}_relax{}", mf.block(block_id).label, skip_id.0);

    let inverted = match kind {
        RelaxKind::BCond { cond } => MachineInst {
            opcode: ArmOpcode::BCond,
            operands: vec![
                MachineOperand::Cond(cond.inverse()),
                MachineOperand::BlockRef(skip_id),
            ],
            def: None,
        },
        RelaxKind::Cbz { invert_to, reg } => MachineInst {
            opcode: invert_to,
            operands: vec![reg, MachineOperand::BlockRef(skip_id)],
            def: None,
        },
        RelaxKind::Tbz {
            invert_to,
            reg,
            bit,
        } => MachineInst {
            opcode: invert_to,
            operands: vec![
                reg,
                MachineOperand::Imm(bit),
                MachineOperand::BlockRef(skip_id),
            ],
            def: None,
        },
    };
    let unconditional_b = MachineInst {
        opcode: ArmOpcode::B,
        operands: vec![MachineOperand::BlockRef(far_target)],
        def: None,
    };

    // Splice them in, preserving any instructions that followed the
    // original branch. Those trailing instructions (uncommon) move
    // into the skip block to keep program order.
    let block_pos = block_position(mf, block_id);
    let trailing = {
        let block = &mut mf.blocks[block_pos];
        let trailing: Vec<MachineInst> = block.insts.drain(inst_idx + 1..).collect();
        block.insts[inst_idx] = inverted;
        block.insts.push(unconditional_b);
        trailing
    };

    let mut skip_block = MachineBlock::new(skip_id, skip_label);
    skip_block.insts = trailing;
    mf.blocks.insert(block_pos + 1, skip_block);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time confirmation that every ArmCond's inverse is
    /// involutive. Easy to typo otherwise.
    #[test]
    fn arm_cond_inverse_is_involutive() {
        for c in [
            ArmCond::Eq,
            ArmCond::Ne,
            ArmCond::Hs,
            ArmCond::Lo,
            ArmCond::Mi,
            ArmCond::Pl,
            ArmCond::Hi,
            ArmCond::Ls,
            ArmCond::Ge,
            ArmCond::Lt,
            ArmCond::Gt,
            ArmCond::Le,
        ] {
            assert_eq!(c.inverse().inverse(), c, "{:?}", c);
        }
    }

    /// In-range branches stay as a single instruction.
    #[test]
    fn in_range_bcond_unchanged() {
        let mut mf = MachineFunction::new("test".into());
        let target = mf.new_block("after");
        let entry_id = mf.blocks[0].id;
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::BCond,
            operands: vec![
                MachineOperand::Cond(ArmCond::Ne),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });
        let _ = entry_id;

        let block_count_before = mf.blocks.len();
        relax_branches(&mut mf);
        assert_eq!(mf.blocks.len(), block_count_before);
        assert_eq!(mf.blocks[0].insts.len(), 1);
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::BCond);
    }

    /// Out-of-range branch expands to a trampoline. We force
    /// overflow by stuffing one block with enough nops to push the
    /// target's offset past ±1MB.
    #[test]
    fn out_of_range_bcond_expands() {
        let mut mf = MachineFunction::new("test".into());
        let target = mf.new_block("after_padding");
        let padding = mf.new_block("padding");

        // Entry: BCond Ne -> target.
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::BCond,
            operands: vec![
                MachineOperand::Cond(ArmCond::Ne),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });
        // Branch into padding so the entry block has at least one
        // successor, then padding stuffs enough Nops to push the
        // `after_padding` label past ±1MB.
        let pad_inst_count = (COND_BRANCH_LIMIT as usize) / 4 + 16;
        let padding_pos = mf.blocks.iter().position(|b| b.id == padding).unwrap();
        mf.blocks[padding_pos].insts = (0..pad_inst_count)
            .map(|_| MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            })
            .collect();
        // Layout order: entry → padding → target. The padding block
        // pushes target's offset out of range from the entry's BCond.
        mf.blocks.swap(1, 2); // ensure entry, padding, target order

        let blocks_before = mf.blocks.len();
        relax_branches(&mut mf);
        // One new skip block was inserted right after entry.
        assert_eq!(mf.blocks.len(), blocks_before + 1);
        // Entry now ends with: BCond(inverted) -> skip; B target.
        let entry = &mf.blocks[0];
        assert_eq!(entry.insts.len(), 2);
        assert_eq!(entry.insts[0].opcode, ArmOpcode::BCond);
        assert!(matches!(
            entry.insts[0].operands[0],
            MachineOperand::Cond(ArmCond::Eq) // inverse of Ne
        ));
        assert_eq!(entry.insts[1].opcode, ArmOpcode::B);
        assert!(matches!(
            entry.insts[1].operands[0],
            MachineOperand::BlockRef(t) if t == target
        ));
    }

    /// In-range cbz stays as a single instruction.
    #[test]
    fn in_range_cbz_unchanged() {
        let mut mf = MachineFunction::new("test".into());
        let target = mf.new_block("after");
        let test_reg = mf.new_vreg(crate::codegen::mir::RegClass::Gp32);
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Cbz,
            operands: vec![
                MachineOperand::VReg(test_reg),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });

        let block_count_before = mf.blocks.len();
        relax_branches(&mut mf);
        assert_eq!(mf.blocks.len(), block_count_before);
        assert_eq!(mf.blocks[0].insts.len(), 1);
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::Cbz);
    }

    /// Out-of-range cbz expands to: cbnz reg, skip; b far. Padding
    /// pushes the target past ±1MB so the original cbz can no longer
    /// reach it directly. The inverted opcode (Cbnz) jumps over the
    /// unconditional `b`, which has the full ±128MB reach and never
    /// itself needs further relaxation.
    #[test]
    fn out_of_range_cbz_expands_to_inverted_skip() {
        let mut mf = MachineFunction::new("test".into());
        let target = mf.new_block("after_padding");
        let padding = mf.new_block("padding");
        let test_reg = mf.new_vreg(crate::codegen::mir::RegClass::Gp32);

        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Cbz,
            operands: vec![
                MachineOperand::VReg(test_reg),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });
        let pad_inst_count = (COND_BRANCH_LIMIT as usize) / 4 + 16;
        let padding_pos = mf.blocks.iter().position(|b| b.id == padding).unwrap();
        mf.blocks[padding_pos].insts = (0..pad_inst_count)
            .map(|_| MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            })
            .collect();
        mf.blocks.swap(1, 2);

        let blocks_before = mf.blocks.len();
        relax_branches(&mut mf);
        assert_eq!(mf.blocks.len(), blocks_before + 1);
        let entry = &mf.blocks[0];
        assert_eq!(entry.insts.len(), 2);
        assert_eq!(entry.insts[0].opcode, ArmOpcode::Cbnz, "cbz inverts to cbnz");
        assert_eq!(entry.insts[1].opcode, ArmOpcode::B);
        assert!(matches!(
            entry.insts[1].operands[0],
            MachineOperand::BlockRef(t) if t == target
        ));
    }

    /// Out-of-range cbnz inverts to cbz on the skip path.
    #[test]
    fn out_of_range_cbnz_expands_to_cbz_skip() {
        let mut mf = MachineFunction::new("test".into());
        let target = mf.new_block("after_padding");
        let padding = mf.new_block("padding");
        let test_reg = mf.new_vreg(crate::codegen::mir::RegClass::Gp64);

        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Cbnz,
            operands: vec![
                MachineOperand::VReg(test_reg),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });
        let pad_inst_count = (COND_BRANCH_LIMIT as usize) / 4 + 16;
        let padding_pos = mf.blocks.iter().position(|b| b.id == padding).unwrap();
        mf.blocks[padding_pos].insts = (0..pad_inst_count)
            .map(|_| MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            })
            .collect();
        mf.blocks.swap(1, 2);

        relax_branches(&mut mf);
        let entry = &mf.blocks[0];
        assert_eq!(entry.insts[0].opcode, ArmOpcode::Cbz, "cbnz inverts to cbz");
    }

    /// In-range tbz stays as a single instruction.
    #[test]
    fn in_range_tbz_unchanged() {
        let mut mf = MachineFunction::new("test".into());
        let target = mf.new_block("after");
        let test_reg = mf.new_vreg(crate::codegen::mir::RegClass::Gp64);
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Tbz,
            operands: vec![
                MachineOperand::VReg(test_reg),
                MachineOperand::Imm(5),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });

        let block_count_before = mf.blocks.len();
        relax_branches(&mut mf);
        assert_eq!(mf.blocks.len(), block_count_before);
        assert_eq!(mf.blocks[0].insts.len(), 1);
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::Tbz);
    }

    /// Out-of-range tbz uses the tighter ±32KB limit and expands. Tbz
    /// is the most range-restricted of the four conditional branches
    /// we emit, so anything past 32K-ish bytes from the branch must
    /// trip relaxation.
    #[test]
    fn out_of_range_tbz_expands_to_tbnz_skip() {
        let mut mf = MachineFunction::new("test".into());
        let target = mf.new_block("after_padding");
        let padding = mf.new_block("padding");
        let test_reg = mf.new_vreg(crate::codegen::mir::RegClass::Gp64);

        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Tbz,
            operands: vec![
                MachineOperand::VReg(test_reg),
                MachineOperand::Imm(7),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });
        // Tbz's ±32KB range is much tighter than the ±1MB cond-branch
        // range — only ~8K nops are needed to overflow.
        let pad_inst_count = (TBZ_BRANCH_LIMIT as usize) / 4 + 16;
        let padding_pos = mf.blocks.iter().position(|b| b.id == padding).unwrap();
        mf.blocks[padding_pos].insts = (0..pad_inst_count)
            .map(|_| MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            })
            .collect();
        mf.blocks.swap(1, 2);

        let blocks_before = mf.blocks.len();
        relax_branches(&mut mf);
        assert_eq!(mf.blocks.len(), blocks_before + 1);
        let entry = &mf.blocks[0];
        assert_eq!(entry.insts.len(), 2);
        assert_eq!(entry.insts[0].opcode, ArmOpcode::Tbnz, "tbz inverts to tbnz");
        // Bit operand survives the rewrite.
        assert!(matches!(entry.insts[0].operands[1], MachineOperand::Imm(7)));
        assert_eq!(entry.insts[1].opcode, ArmOpcode::B);
    }
}
