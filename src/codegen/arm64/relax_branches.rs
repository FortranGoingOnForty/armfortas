//! ARM64 local-branch range relaxation.
//!
//! `B.cond`/`CBZ` carry a 19-bit signed PC-relative immediate (±1MB),
//! `TBZ` carries 14 bits (±32KB), and local `B` carries 26 bits
//! (±128MB). A function can exceed any of those spans, and relaxing a
//! conditional branch can itself introduce an out-of-range local `B`.
//!
//! This pass runs after register allocation and tail-call optimization,
//! before assembly emission. It walks the function's blocks in linear
//! order and computes the exact emit-time byte offset of every block.
//! Out-of-range conditional branches become an inverted short branch
//! around an unconditional branch:
//!
//! ```text
//! Before:                     After (when target is too far):
//!   b.cond  far_label             b.{!cond}  skip       ; one branch over
//!                                 b          far_label
//!                               skip:                   ; falls through
//! ```
//!
//! An out-of-range local `B` becomes a position-independent veneer
//! through the reserved intra-procedure-call scratch register:
//!
//! ```text
//!   adrp x16, far_label@PAGE
//!   add  x16, x16, far_label@PAGEOFF
//!   br   x16
//! ```
//!
//! Every rewrite grows the layout and can expose another overflow, so
//! the pass iterates to a fixed point under a structural bound derived
//! from the original branch population. Exceeding that bound produces
//! a release-build ICE rather than invalid machine code.

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

/// Conservative branch26 reach for a local unconditional `B`. The
/// architectural signed immediate is scaled by four (±128MB); keep the
/// same 64-byte growth margin used by the conditional forms.
const B_BRANCH_LIMIT: i64 = (1 << 27) - 64;

#[derive(Clone, Copy)]
struct BranchLimits {
    cond: i64,
    tbz: i64,
    unconditional: i64,
}

const PRODUCTION_BRANCH_LIMITS: BranchLimits = BranchLimits {
    cond: COND_BRANCH_LIMIT,
    tbz: TBZ_BRANCH_LIMIT,
    unconditional: B_BRANCH_LIMIT,
};

/// Run local-branch relaxation on a machine function. Idempotent once
/// every relaxable branch is in range or uses a long veneer.
pub fn relax_branches(mf: &mut MachineFunction) {
    relax_branches_with_limits(mf, PRODUCTION_BRANCH_LIMITS);
}

fn relax_branches_with_limits(mf: &mut MachineFunction, limits: BranchLimits) {
    // A conditional branch can change once into a short inverted branch
    // plus B, and that generated B can change once more into a veneer.
    // An original local B can change only once. The inclusive loop bound
    // reserves a final pass that must observe no changes.
    let mut conditional_count = 0usize;
    let mut unconditional_count = 0usize;
    for inst in mf.blocks.iter().flat_map(|block| &block.insts) {
        if let Some((_, _, kind)) = relaxable_branch(inst, limits) {
            if matches!(kind, RelaxKind::B) {
                unconditional_count += 1;
            } else {
                conditional_count += 1;
            }
        }
    }
    let max_changing_passes = conditional_count
        .saturating_mul(2)
        .saturating_add(unconditional_count);
    for _ in 0..=max_changing_passes {
        if !relax_once_with_limits(mf, limits) {
            return;
        }
    }
    panic!(
        "branch relaxation did not converge for function {} after exceeding its structural bound of {} changing passes ({} conditional, {} local unconditional)",
        mf.name, max_changing_passes, conditional_count, unconditional_count
    );
}

/// Production-limit wrapper used by direct pass-level tests.
fn relax_once(mf: &mut MachineFunction) -> bool {
    relax_once_with_limits(mf, PRODUCTION_BRANCH_LIMITS)
}

/// Single relaxation pass. Returns `true` when at least one branch was
/// expanded (caller should iterate); `false` at the fixed point.
fn relax_once_with_limits(mf: &mut MachineFunction, limits: BranchLimits) -> bool {
    // Compute byte offsets for every block label using the actual
    // emit-time instruction count. Several MIR opcodes lower to
    // multiple ARM64 instructions (AdrpLdr / AdrpAdd / AdrpGotLdr, prologue +
    // stack-probe sequences, large-immediate movz/movk chains, etc.),
    // so a flat 4-bytes-per-MachineInst estimate would systematically
    // under-shoot the real offsets in functions like stdlib_slaruv
    // and miss far branches that need relaxation.
    let block_offsets = compute_block_offsets(mf);
    let mut overflows = collect_overflows(mf, &block_offsets, limits);

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
        expand_overflow(mf, site);
    }

    true
}

fn collect_overflows(
    mf: &MachineFunction,
    block_offsets: &std::collections::HashMap<MBlockId, i64>,
    limits: BranchLimits,
) -> Vec<OverflowSite> {
    // Collect overflow sites before mutating anything. We also record
    // the per-instruction prefix offset within each block so we can
    // skip past wide-emit insts that precede a branch.
    let mut overflows: Vec<OverflowSite> = Vec::new();
    for block in &mf.blocks {
        let block_offset = block_offsets[&block.id];
        let mut running = 0i64;
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            if let Some((target, limit, kind)) = relaxable_branch(inst, limits) {
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
    overflows
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
    /// `b far` → `adrp/add/br` via reserved IP0/x16.
    B,
    /// `b.cond far` → `b.{!cond} skip; b far`
    BCond { cond: ArmCond },
    /// `cbz/cbnz reg far` → `cbnz/cbz reg skip; b far`
    Cbz {
        invert_to: ArmOpcode,
        reg: MachineOperand,
    },
    /// `tbz/tbnz reg, #bit far` → `tbnz/tbz reg, #bit skip; b far`
    Tbz {
        invert_to: ArmOpcode,
        reg: MachineOperand,
        bit: i64,
    },
}

/// Inspect a local direct branch and return its target, configured range,
/// and rewrite kind. External `B`/`BL` relocations remain linker-owned.
fn relaxable_branch(
    inst: &MachineInst,
    limits: BranchLimits,
) -> Option<(MBlockId, i64, RelaxKind)> {
    match inst.opcode {
        ArmOpcode::B => {
            let target = match inst.operands.first()? {
                MachineOperand::BlockRef(id) => *id,
                _ => return None,
            };
            Some((target, limits.unconditional, RelaxKind::B))
        }
        ArmOpcode::BCond => {
            let target = bcond_target(inst)?;
            let cond = bcond_cond(inst)?;
            Some((target, limits.cond, RelaxKind::BCond { cond }))
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
            Some((target, limits.cond, RelaxKind::Cbz { invert_to, reg }))
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
                limits.tbz,
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

fn expand_overflow(mf: &mut MachineFunction, site: OverflowSite) {
    let OverflowSite {
        block_id,
        inst_idx,
        target: far_target,
        kind,
    } = site;

    match kind {
        RelaxKind::B => {
            mf.ensure_long_branch_label(far_target);
            mf.block_mut(block_id).insts[inst_idx] = MachineInst {
                opcode: ArmOpcode::BLong,
                operands: vec![MachineOperand::BlockRef(far_target)],
                def: None,
            };
        }
        conditional => {
            expand_conditional_to_trampoline(mf, block_id, inst_idx, far_target, conditional)
        }
    }
}

/// Replace an out-of-range conditional branch with an inverted short
/// branch over a local `B`. A later relaxation pass widens that `B` if
/// the original target also lies outside branch26 range.
fn expand_conditional_to_trampoline(
    mf: &mut MachineFunction,
    block_id: MBlockId,
    inst_idx: usize,
    far_target: MBlockId,
    kind: RelaxKind,
) {
    // Allocate a fresh skip-block id without disturbing any other
    // bookkeeping. We can't call `mf.new_block` directly because it
    // pushes onto the back of the vec; we want the skip block
    // physically adjacent to the original block (so its label
    // fall-through reaches the original successor).
    let skip_id = MBlockId(mf.next_block_id());
    let skip_label = format!("{}_relax{}", mf.block(block_id).label, skip_id.0);

    let inverted = match kind {
        RelaxKind::B => unreachable!("unconditional branches do not use conditional trampolines"),
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

    #[test]
    fn far_unconditional_branch_is_reported_for_relaxation() {
        let mut mf = MachineFunction::new("far_b".into());
        let source = mf.blocks[0].id;
        let target = mf.new_block("target");
        mf.block_mut(source).insts.push(MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::BlockRef(target)],
            def: None,
        });

        // Inject the exact layout fact instead of allocating a 128 MiB MIR
        // fixture: branch26 is signed imm26 * 4, and the production safety
        // margin ends 64 bytes below its architectural positive limit.
        let mut offsets = compute_block_offsets(&mf);
        offsets.insert(target, (1_i64 << 27) - 64 + 4);

        assert_eq!(
            collect_overflows(&mf, &offsets, PRODUCTION_BRANCH_LIMITS).len(),
            1,
            "a local B beyond the safe branch26 range must reach relaxation"
        );
    }

    #[test]
    fn branch26_safe_boundary_is_checked_in_both_directions() {
        let mut mf = MachineFunction::new("branch26_bounds".into());
        let source = mf.blocks[0].id;
        let target = mf.new_block("Lbranch26_target");
        mf.block_mut(source).insts.push(MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::BlockRef(target)],
            def: None,
        });

        let overflow_count = |delta: i64| {
            let source_offset = B_BRANCH_LIMIT + 128;
            let mut offsets = std::collections::HashMap::new();
            offsets.insert(source, source_offset);
            offsets.insert(target, source_offset + delta);
            collect_overflows(&mf, &offsets, PRODUCTION_BRANCH_LIMITS).len()
        };

        assert_eq!(overflow_count(B_BRANCH_LIMIT), 0);
        assert_eq!(overflow_count(-B_BRANCH_LIMIT), 0);
        assert_eq!(overflow_count(B_BRANCH_LIMIT + 4), 1);
        assert_eq!(overflow_count(-B_BRANCH_LIMIT - 4), 1);
    }

    #[test]
    fn far_unconditional_branch_uses_a_pic_veneer() {
        let mut mf = MachineFunction::new("far_b_veneer".into());
        let source = mf.blocks[0].id;
        let padding = mf.new_block("Lfar_b_padding");
        let target = mf.new_block("Lfar_b_target");
        mf.block_mut(source).insts.push(MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::BlockRef(target)],
            def: None,
        });
        mf.block_mut(padding).insts = (0..2)
            .map(|_| MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            })
            .collect();

        let limits = BranchLimits {
            unconditional: 8,
            ..PRODUCTION_BRANCH_LIMITS
        };
        let initial_offsets = compute_block_offsets(&mf);
        assert_eq!(initial_offsets[&target] - initial_offsets[&source], 12);

        relax_branches_with_limits(&mut mf, limits);

        assert_eq!(mf.block(source).insts[0].opcode, ArmOpcode::BLong);
        assert!(
            !relax_once_with_limits(&mut mf, limits),
            "the veneer must be the fixed point"
        );

        let long_label = mf
            .long_branch_label(target)
            .expect("relaxation must retain a local relocation anchor");
        let assembly = crate::codegen::arm64::emit::emit_function(&mf);
        assert!(assembly.contains(&format!("adrp x16, {long_label}@PAGE")));
        assert!(assembly.contains(&format!("add x16, x16, {long_label}@PAGEOFF")));
        assert!(assembly.contains("br x16"));
        let short_branch = format!("b {long_label}");
        assert!(
            !assembly.lines().any(|line| line.trim() == short_branch),
            "the out-of-range branch26 instruction must be gone"
        );

        let source = format!(".section __TEXT,__text,regular,pure_instructions\n{assembly}");
        let object = afs_as::assemble::assemble_source(&source)
            .expect("afs-as must encode the long local-branch veneer");
        let mut relocation_types: Vec<_> = object
            .sections
            .iter()
            .flat_map(|section| &section.relocations)
            .map(|relocation| relocation.reloc_type)
            .collect();
        relocation_types.sort_unstable();
        assert_eq!(
            relocation_types,
            vec![
                afs_as::macho::ARM64_RELOC_PAGE21,
                afs_as::macho::ARM64_RELOC_PAGEOFF12,
            ],
            "the veneer needs only PIC page relocations, never branch26"
        );

        let mut first_bytes = Vec::new();
        afs_as::macho::write_macho(&object, &mut first_bytes)
            .expect("afs-as must serialize the veneer object");
        let repeated_object = afs_as::assemble::assemble_source(&source)
            .expect("repeated veneer assembly must succeed");
        let mut repeated_bytes = Vec::new();
        afs_as::macho::write_macho(&repeated_object, &mut repeated_bytes)
            .expect("afs-as must serialize the repeated veneer object");
        assert_eq!(
            first_bytes, repeated_bytes,
            "the long-branch object must be deterministic"
        );
    }

    #[test]
    fn far_conditional_trampoline_can_widen_its_generated_b() {
        let mut mf = MachineFunction::new("far_cond_then_b".into());
        let source = mf.blocks[0].id;
        let padding = mf.new_block("Lfar_cond_padding");
        let target = mf.new_block("Lfar_cond_target");
        mf.block_mut(source).insts.push(MachineInst {
            opcode: ArmOpcode::BCond,
            operands: vec![
                MachineOperand::Cond(ArmCond::Ne),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        });
        mf.block_mut(padding).insts = (0..16)
            .map(|_| MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            })
            .collect();

        let limits = BranchLimits {
            cond: 64,
            tbz: 64,
            unconditional: 8,
        };
        let initial_offsets = compute_block_offsets(&mf);
        assert_eq!(initial_offsets[&target] - initial_offsets[&source], 68);

        let blocks_before = mf.blocks.len();
        relax_branches_with_limits(&mut mf, limits);

        assert_eq!(mf.blocks.len(), blocks_before + 1);
        assert_eq!(mf.block(source).insts[0].opcode, ArmOpcode::BCond);
        assert_eq!(mf.block(source).insts[1].opcode, ArmOpcode::BLong);
        assert!(
            !relax_once_with_limits(&mut mf, limits),
            "conditional relaxation plus B widening must reach a fixed point"
        );
    }

    #[test]
    fn external_tail_branch_remains_linker_owned() {
        let mut mf = MachineFunction::new("external_tail".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::Extern("callee".into())],
            def: None,
        });
        let limits = BranchLimits {
            unconditional: 0,
            ..PRODUCTION_BRANCH_LIMITS
        };

        relax_branches_with_limits(&mut mf, limits);

        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::B);
        assert!(crate::codegen::arm64::emit::emit_function(&mf).contains("b _callee"));
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
    /// unconditional `b`; if that `b` also exceeds branch26 reach, a
    /// later pass widens it to the PIC veneer.
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
        assert_eq!(
            entry.insts[0].opcode,
            ArmOpcode::Cbnz,
            "cbz inverts to cbnz"
        );
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
        assert_eq!(
            entry.insts[0].opcode,
            ArmOpcode::Tbnz,
            "tbz inverts to tbnz"
        );
        // Bit operand survives the rewrite.
        assert!(matches!(entry.insts[0].operands[1], MachineOperand::Imm(7)));
        assert_eq!(entry.insts[1].opcode, ArmOpcode::B);
    }

    #[test]
    fn four_stage_overflow_cascade_reaches_a_fixed_point() {
        let mut mf = MachineFunction::new("four_stage_cascade".into());
        let branch4 = mf.blocks[0].id;
        let branch3 = mf.new_block("branch3");
        let branch2 = mf.new_block("branch2");
        let branch1 = mf.new_block("branch1");
        let padding = mf.new_block("padding");
        let target4 = mf.new_block("target4");
        let target3 = mf.new_block("target3");
        let target2 = mf.new_block("target2");
        let target1 = mf.new_block("target1");

        let branch_to = |target| MachineInst {
            opcode: ArmOpcode::BCond,
            operands: vec![
                MachineOperand::Cond(ArmCond::Ne),
                MachineOperand::BlockRef(target),
            ],
            def: None,
        };
        for (source, target) in [
            (branch4, target4),
            (branch3, target3),
            (branch2, target2),
            (branch1, target1),
        ] {
            mf.block_mut(source).insts.push(branch_to(target));
        }

        // Before relaxation the four branch deltas are, in layout order,
        // LIMIT-8, LIMIT-4, LIMIT, and LIMIT+4. Expanding the last branch
        // adds four bytes between every earlier branch and its target, so
        // exactly one additional branch overflows on each following pass.
        let pad_inst_count = ((COND_BRANCH_LIMIT - 24) / 4) as usize;
        mf.block_mut(padding).insts = (0..pad_inst_count)
            .map(|_| MachineInst {
                opcode: ArmOpcode::Nop,
                operands: vec![],
                def: None,
            })
            .collect();
        for target in [target4, target3, target2] {
            mf.block_mut(target).insts = vec![
                MachineInst {
                    opcode: ArmOpcode::Nop,
                    operands: vec![],
                    def: None,
                },
                MachineInst {
                    opcode: ArmOpcode::Nop,
                    operands: vec![],
                    def: None,
                },
            ];
        }

        let offsets = compute_block_offsets(&mf);
        let initial_deltas: Vec<_> = [
            (branch4, target4),
            (branch3, target3),
            (branch2, target2),
            (branch1, target1),
        ]
        .into_iter()
        .map(|(source, target)| offsets[&target] - offsets[&source])
        .collect();
        assert_eq!(
            initial_deltas,
            [
                COND_BRANCH_LIMIT - 8,
                COND_BRANCH_LIMIT - 4,
                COND_BRANCH_LIMIT,
                COND_BRANCH_LIMIT + 4,
            ],
            "fixture must start with exactly one overflowing branch"
        );

        let mut pass_probe = mf.clone();
        for pass in 1..=4 {
            let blocks_before = pass_probe.blocks.len();
            assert!(relax_once(&mut pass_probe), "pass {pass} must change");
            assert_eq!(
                pass_probe.blocks.len(),
                blocks_before + 1,
                "pass {pass} must expose exactly one new overflow"
            );
        }
        assert!(
            !relax_once(&mut pass_probe),
            "the fifth pass must be the no-change convergence check"
        );

        let blocks_before = mf.blocks.len();
        relax_branches(&mut mf);

        assert_eq!(mf.blocks.len(), blocks_before + 4);
        assert_eq!(
            mf.blocks
                .iter()
                .flat_map(|block| &block.insts)
                .filter(|inst| inst.opcode == ArmOpcode::B)
                .count(),
            4,
            "each cascading conditional branch should have one far trampoline"
        );
        assert!(
            !relax_once(&mut mf),
            "the four-stage cascade should already be at a fixed point"
        );

        let probed_asm = crate::codegen::arm64::emit::emit_function(&pass_probe);
        let relaxed_asm = crate::codegen::arm64::emit::emit_function(&mf);
        assert_eq!(
            relaxed_asm, probed_asm,
            "the public fixed-point driver must produce the same layout as explicit passes"
        );

        let source = format!(".section __TEXT,__text,regular,pure_instructions\n{relaxed_asm}");
        let encode = || {
            let object = afs_as::assemble::assemble_source(&source)
                .expect("afs-as must encode every branch in the relaxed cascade");
            let mut bytes = Vec::new();
            afs_as::macho::write_macho(&object, &mut bytes)
                .expect("afs-as must serialize the relaxed cascade as Mach-O");
            bytes
        };
        assert_eq!(
            encode(),
            encode(),
            "identical relaxed assembly must produce deterministic Mach-O bytes"
        );
    }
}
