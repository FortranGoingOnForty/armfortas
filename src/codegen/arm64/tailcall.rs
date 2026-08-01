//! Tail call optimization (post-regalloc peephole).
//!
//! After register allocation and callee-save insertion, the machine code for
//! a call in tail position looks like:
//!
//! ```text
//!   ; arg setup (MOV xi, …)
//!   Bl _callee
//!   ; callee-save restores recorded by callee-save insertion — zero or more
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
//! * Calls with stack-passed arguments are rejected. Their stores are relative
//!   to this function's allocated SP and would not survive frame teardown.
//! * LdpPost restores x29 (our FP) and x30 (our LR).  After it fires, LR
//!   holds our *caller's* return address.  When _callee executes its own
//!   RET, it returns to *our* caller directly — exactly what TCO requires.
//! * We only recognize this pattern when there are **no instructions between
//!   Bl and the exact callee-restore cluster recorded on the machine
//!   function**. Generic loads are not assumed to be restores because result
//!   marshalling and spill reloads use the same opcodes.
//! * Gate: we don't fire on non-void calls where a non-trivial result-capture
//!   sequence remains (e.g., `MOV x1, x0`) — those are left alone.

use super::mir::{ArmOpcode, MBlockId, MachineFunction, MachineInst, MachineOperand, PhysReg};
use std::collections::{HashMap, HashSet, VecDeque};

/// Run tail call optimization on a single machine function.
///
/// Safe to call at any optimization level; the transformation never changes
/// visible behavior and is always a code-size win (removes one instruction).
pub fn tail_call_opt(mf: &mut MachineFunction) {
    let frame_taint_at_entry = frame_taint_entry_states(mf);
    let callee_save_slots = mf.callee_save_slots.clone();
    for (block_idx, block) in mf.blocks.iter_mut().enumerate() {
        let n = block.insts.len();
        if n < 2 {
            continue;
        }

        // Epilogue is always `LdpPost; Ret` at the very end.
        if block.insts[n - 1].opcode != ArmOpcode::Ret {
            continue;
        }
        if block.insts[n - 2].opcode != ArmOpcode::LdpPost {
            continue;
        }

        let ldp_idx = n - 2;

        // Walk backwards from just before LdpPost, skipping only the exact
        // restores emitted for this function's callee-save slots.
        let mut bl_candidate = ldp_idx;
        while bl_candidate > 0 {
            bl_candidate -= 1;
            let inst = &block.insts[bl_candidate];
            if is_callee_save_restore(inst, &callee_save_slots) {
                continue;
            }
            if inst.opcode == ArmOpcode::Bl {
                break;
            }
            bl_candidate = usize::MAX;
            break;
        }

        // Sentinel or scanned to index 0 without finding Bl.
        if bl_candidate == usize::MAX {
            continue;
        }
        if block.insts[bl_candidate].opcode != ArmOpcode::Bl {
            continue;
        }
        if call_uses_outgoing_stack(&block.insts[bl_candidate]) {
            continue;
        }

        // SAFETY: reject TCO when any argument register (x0–x7) holds a value
        // derived from our frame pointer (e.g. a pointer to a stack-allocated
        // local / derived-type struct).  After the epilogue tears down our
        // frame, the callee's prologue reuses that memory; any pointer into it
        // becomes dangling.  Taint analysis: track GP registers set from
        // frame-relative address materialization and propagated through
        // MovReg / AddReg / AddImm / Mul / CselReg.  If any x0–x7 is tainted,
        // the tail call is unsafe.
        if has_frame_derived_arg(
            &frame_taint_at_entry[block_idx],
            &block.insts[..bl_candidate],
        ) {
            continue;
        }

        // Extract the call target from the Bl operand.
        let label = match block.insts[bl_candidate].operands.first() {
            Some(MachineOperand::Extern(s)) => s.clone(),
            _ => continue, // indirect call or unexpected operand — skip
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

fn call_uses_outgoing_stack(inst: &MachineInst) -> bool {
    matches!(inst.operands.get(1), Some(MachineOperand::Imm(bytes)) if *bytes > 0)
}

fn is_callee_save_restore(inst: &MachineInst, slots: &[(PhysReg, i32)]) -> bool {
    let phys = |index| match inst.operands.get(index) {
        Some(MachineOperand::PhysReg(reg)) => Some(*reg),
        _ => None,
    };
    let imm = |index| match inst.operands.get(index) {
        Some(MachineOperand::Imm(value)) => i32::try_from(*value).ok(),
        _ => None,
    };
    let has_slot = |reg, offset| slots.contains(&(reg, offset));

    match inst.opcode {
        ArmOpcode::LdrImm | ArmOpcode::LdrFpImm => {
            phys(1) == Some(PhysReg::FP)
                && phys(0)
                    .zip(imm(2))
                    .is_some_and(|(reg, offset)| has_slot(reg, offset))
        }
        ArmOpcode::LdpOffset => {
            phys(2) == Some(PhysReg::FP)
                && phys(0)
                    .zip(phys(1))
                    .zip(imm(3))
                    .is_some_and(|((low, high), offset)| {
                        has_slot(low, offset)
                            && offset
                                .checked_add(8)
                                .is_some_and(|high_offset| has_slot(high, high_offset))
                    })
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Safety helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameAddress {
    Exact(i64),
    Unknown,
}

impl FrameAddress {
    fn add(self, offset: i64) -> Self {
        match self {
            Self::Exact(base) => base
                .checked_add(offset)
                .map(Self::Exact)
                .unwrap_or(Self::Unknown),
            Self::Unknown => Self::Unknown,
        }
    }

    fn sub(self, offset: i64) -> Self {
        match self {
            Self::Exact(base) => base
                .checked_sub(offset)
                .map(Self::Exact)
                .unwrap_or(Self::Unknown),
            Self::Unknown => Self::Unknown,
        }
    }

    fn merge(self, other: Self) -> Self {
        if self == other {
            self
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FrameTaintState {
    /// GP register numbers whose current value may derive from this frame.
    tainted_regs: HashSet<u8>,
    /// Exact FP-relative slots that may contain a frame-derived pointer.
    tainted_slots: HashSet<i64>,
    /// Registers that may address this frame, with exact offsets when known.
    frame_addr_regs: HashMap<u8, FrameAddress>,
    /// A frame-derived pointer was stored through an imprecise frame address.
    unknown_tainted_slot: bool,
}

impl FrameTaintState {
    fn merge_from(&mut self, other: &Self) -> bool {
        let mut changed = false;

        let old_reg_count = self.tainted_regs.len();
        self.tainted_regs.extend(&other.tainted_regs);
        changed |= self.tainted_regs.len() != old_reg_count;

        let old_slot_count = self.tainted_slots.len();
        self.tainted_slots.extend(&other.tainted_slots);
        changed |= self.tainted_slots.len() != old_slot_count;

        for (&reg, &incoming) in &other.frame_addr_regs {
            match self.frame_addr_regs.get_mut(&reg) {
                Some(current) => {
                    let merged = current.merge(incoming);
                    if *current != merged {
                        *current = merged;
                        changed = true;
                    }
                }
                None => {
                    self.frame_addr_regs.insert(reg, incoming);
                    changed = true;
                }
            }
        }

        if other.unknown_tainted_slot && !self.unknown_tainted_slot {
            self.unknown_tainted_slot = true;
            changed = true;
        }

        changed
    }

    fn has_frame_derived_argument(&self) -> bool {
        (0u8..8).any(|reg| self.tainted_regs.contains(&reg))
    }
}

fn frame_taint_entry_states(mf: &MachineFunction) -> Vec<FrameTaintState> {
    let block_indices: HashMap<MBlockId, usize> = mf
        .blocks
        .iter()
        .enumerate()
        .map(|(idx, block)| (block.id, idx))
        .collect();
    let mut successors = vec![Vec::new(); mf.blocks.len()];

    for (block_idx, block) in mf.blocks.iter().enumerate() {
        for inst in &block.insts {
            if let Some(target) = local_branch_target(inst) {
                if let Some(&successor) = block_indices.get(&target) {
                    if !successors[block_idx].contains(&successor) {
                        successors[block_idx].push(successor);
                    }
                }
            }
        }

        let ends_control_flow = block.insts.last().is_some_and(|inst| {
            matches!(
                inst.opcode,
                ArmOpcode::B | ArmOpcode::BLong | ArmOpcode::Ret
            )
        });
        if !ends_control_flow && block_idx + 1 < mf.blocks.len() {
            let fallthrough = block_idx + 1;
            if !successors[block_idx].contains(&fallthrough) {
                successors[block_idx].push(fallthrough);
            }
        }
    }

    let mut entries = vec![FrameTaintState::default(); mf.blocks.len()];
    if mf.blocks.is_empty() {
        return entries;
    }

    let mut reachable = vec![false; mf.blocks.len()];
    let mut queued = vec![false; mf.blocks.len()];
    let mut worklist = VecDeque::from([0usize]);
    reachable[0] = true;
    queued[0] = true;

    while let Some(block_idx) = worklist.pop_front() {
        queued[block_idx] = false;
        let mut outgoing = entries[block_idx].clone();
        propagate_frame_taint(&mut outgoing, &mf.blocks[block_idx].insts);

        for &successor in &successors[block_idx] {
            let changed = if reachable[successor] {
                entries[successor].merge_from(&outgoing)
            } else {
                entries[successor] = outgoing.clone();
                reachable[successor] = true;
                true
            };
            if changed && !queued[successor] {
                worklist.push_back(successor);
                queued[successor] = true;
            }
        }
    }

    entries
}

fn local_branch_target(inst: &MachineInst) -> Option<MBlockId> {
    let target_index = match inst.opcode {
        ArmOpcode::B | ArmOpcode::BLong => 0,
        ArmOpcode::BCond | ArmOpcode::Cbz | ArmOpcode::Cbnz => 1,
        ArmOpcode::Tbz | ArmOpcode::Tbnz => 2,
        _ => return None,
    };
    match inst.operands.get(target_index) {
        Some(MachineOperand::BlockRef(target)) => Some(*target),
        _ => None,
    }
}

/// Returns true if any GP argument register (x0–x7) contains a frame-derived
/// pointer at the point of the Bl.
///
/// "Frame-derived" means the register was set — directly or transitively — from
/// an `add`/`sub` relative to x29 (local-address materialization).
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
fn has_frame_derived_arg(entry: &FrameTaintState, insts: &[MachineInst]) -> bool {
    let mut state = entry.clone();
    propagate_frame_taint(&mut state, insts);
    state.has_frame_derived_argument()
}

fn propagate_frame_taint(state: &mut FrameTaintState, insts: &[MachineInst]) {
    for inst in insts {
        let source_frame_address = op_gp(inst, 1)
            .and_then(|source| state.frame_addr_regs.get(&source))
            .copied();
        let frame_access = match inst.opcode {
            ArmOpcode::StrImm | ArmOpcode::LdrImm => {
                effective_frame_slot_offset(inst, 1, 2, &state.frame_addr_regs)
            }
            _ => None,
        };
        if let Some(dst) = written_gp_reg(inst) {
            state.frame_addr_regs.remove(&dst);
        }
        match inst.opcode {
            // sub xN, x29, #imm  →  xN holds a frame-relative address.
            ArmOpcode::SubImm if op_is_fp(inst, 1) => {
                if let Some(n) = op_gp(inst, 0) {
                    state.tainted_regs.insert(n);
                    let address = op_imm(inst, 2)
                        .and_then(i64::checked_neg)
                        .map(FrameAddress::Exact)
                        .unwrap_or(FrameAddress::Unknown);
                    state.frame_addr_regs.insert(n, address);
                }
            }
            // add xN, x29, #imm  →  xN holds a frame-relative address.
            ArmOpcode::AddImm if op_is_fp(inst, 1) => {
                if let Some(n) = op_gp(inst, 0) {
                    state.tainted_regs.insert(n);
                    let address = op_imm(inst, 2)
                        .map(FrameAddress::Exact)
                        .unwrap_or(FrameAddress::Unknown);
                    state.frame_addr_regs.insert(n, address);
                }
            }
            // add xN, xM, #imm where xM is a known frame address.
            ArmOpcode::AddImm => {
                if let (Some(dst), Some(src), Some(imm)) =
                    (op_gp(inst, 0), op_gp(inst, 1), op_imm(inst, 2))
                {
                    if let Some(base) = source_frame_address {
                        state.tainted_regs.insert(dst);
                        state.frame_addr_regs.insert(dst, base.add(imm));
                    } else if state.tainted_regs.contains(&src) {
                        state.tainted_regs.insert(dst);
                        state.frame_addr_regs.insert(dst, FrameAddress::Unknown);
                    }
                }
            }
            // sub xN, xM, #imm where xM is a known frame address.
            ArmOpcode::SubImm => {
                if let (Some(dst), Some(src), Some(imm)) =
                    (op_gp(inst, 0), op_gp(inst, 1), op_imm(inst, 2))
                {
                    if let Some(base) = source_frame_address {
                        state.tainted_regs.insert(dst);
                        state.frame_addr_regs.insert(dst, base.sub(imm));
                    } else if state.tainted_regs.contains(&src) {
                        state.tainted_regs.insert(dst);
                        state.frame_addr_regs.insert(dst, FrameAddress::Unknown);
                    }
                }
            }
            // add xN, xM, xP  (GEP: propagate taint from either source)
            ArmOpcode::AddReg
                if op_gp(inst, 1).is_some_and(|n| state.tainted_regs.contains(&n))
                    || op_gp(inst, 2).is_some_and(|n| state.tainted_regs.contains(&n)) =>
            {
                if let Some(n) = op_gp(inst, 0) {
                    state.tainted_regs.insert(n);
                    state.frame_addr_regs.insert(n, FrameAddress::Unknown);
                }
            }
            // mov xN, xM  (register copy — propagates taint to arg reg)
            ArmOpcode::MovReg
                if op_gp(inst, 1).is_some_and(|n| state.tainted_regs.contains(&n)) =>
            {
                if let Some(n) = op_gp(inst, 0) {
                    state.tainted_regs.insert(n);
                    state
                        .frame_addr_regs
                        .insert(n, source_frame_address.unwrap_or(FrameAddress::Unknown));
                }
            }
            // mul xN, xM, xP  (index computation in GEP; conservative)
            ArmOpcode::Mul
                if op_gp(inst, 1).is_some_and(|n| state.tainted_regs.contains(&n))
                    || op_gp(inst, 2).is_some_and(|n| state.tainted_regs.contains(&n)) =>
            {
                if let Some(n) = op_gp(inst, 0) {
                    state.tainted_regs.insert(n);
                    state.frame_addr_regs.insert(n, FrameAddress::Unknown);
                }
            }
            // csel xN, xM, xP, cond — either selectable value can reach the
            // destination at runtime, so taint from either source is enough
            // to make the result frame-derived.
            ArmOpcode::CselReg
                if op_gp(inst, 1).is_some_and(|n| state.tainted_regs.contains(&n))
                    || op_gp(inst, 2).is_some_and(|n| state.tainted_regs.contains(&n)) =>
            {
                if let Some(n) = op_gp(inst, 0) {
                    state.tainted_regs.insert(n);
                    state.frame_addr_regs.insert(n, FrameAddress::Unknown);
                }
            }
            // str xN, [x29, #off] — if xN is tainted, the slot becomes tainted.
            ArmOpcode::StrImm
                if op_gp(inst, 0).is_some_and(|n| state.tainted_regs.contains(&n))
                    && frame_access.is_some() =>
            {
                match frame_access {
                    Some(FrameAddress::Exact(offset)) => {
                        state.tainted_slots.insert(offset);
                    }
                    Some(FrameAddress::Unknown) => state.unknown_tainted_slot = true,
                    None => {}
                }
            }
            // ldr xN, [frame] — if the slot is known tainted, xN becomes
            // tainted. Also conservatively reject tail calls when any 64-bit
            // GP register is reloaded from our frame in the tail block: the
            // slot may have been populated in a predecessor with an escaped
            // local address, then copied into x0–x7 later in the block.
            ArmOpcode::LdrImm => {
                if let Some(address) = frame_access {
                    if let Some(n) = op_gp(inst, 0) {
                        let may_hold_frame_pointer = match address {
                            FrameAddress::Exact(offset) => {
                                state.unknown_tainted_slot
                                    || state.tainted_slots.contains(&offset)
                                    || n <= 30
                            }
                            FrameAddress::Unknown => true,
                        };
                        if may_hold_frame_pointer {
                            state.tainted_regs.insert(n);
                            state.frame_addr_regs.insert(n, FrameAddress::Unknown);
                        }
                    }
                }
            }
            _ => {}
        }
    }
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

/// Integer immediate operand at `idx`, or None.
#[inline]
fn op_imm(inst: &MachineInst, idx: usize) -> Option<i64> {
    match inst.operands.get(idx)? {
        MachineOperand::Imm(v) => Some(*v),
        MachineOperand::FrameSlot(v) => Some(*v as i64),
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

/// Effective FP-relative offset addressed by `[base, #off]`, where `base` is
/// either FP directly or a GP register previously materialized from FP.
#[inline]
fn effective_frame_slot_offset(
    inst: &MachineInst,
    base_idx: usize,
    off_idx: usize,
    frame_addr_regs: &HashMap<u8, FrameAddress>,
) -> Option<FrameAddress> {
    let off = op_imm(inst, off_idx).unwrap_or(0);
    match inst.operands.get(base_idx)? {
        MachineOperand::PhysReg(p) if *p == PhysReg::FP => Some(FrameAddress::Exact(off)),
        MachineOperand::PhysReg(PhysReg::Gp(n)) => {
            frame_addr_regs.get(n).copied().map(|base| base.add(off))
        }
        _ => None,
    }
}

/// GP register written by this instruction, if operand 0 is a GP destination.
#[inline]
fn written_gp_reg(inst: &MachineInst) -> Option<u8> {
    match inst.opcode {
        ArmOpcode::AddReg
        | ArmOpcode::AddImm
        | ArmOpcode::SubReg
        | ArmOpcode::SubImm
        | ArmOpcode::Mul
        | ArmOpcode::MovReg
        | ArmOpcode::CselReg
        | ArmOpcode::LdrImm => op_gp(inst, 0),
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
        MachineBlock {
            label: "test".into(),
            insts,
            id: MBlockId(0),
        }
    }

    fn bl(label: &str) -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![MachineOperand::Extern(label.into())],
            def: None,
        }
    }

    fn bl_with_stack_args(label: &str, bytes: i64) -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![
                MachineOperand::Extern(label.into()),
                MachineOperand::Imm(bytes),
            ],
            def: None,
        }
    }

    fn branch(target: u32) -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::BlockRef(MBlockId(target))],
            def: None,
        }
    }

    fn conditional_branch(target: u32) -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::BCond,
            operands: vec![
                MachineOperand::Cond(ArmCond::Ne),
                MachineOperand::BlockRef(MBlockId(target)),
            ],
            def: None,
        }
    }

    fn frame_address(destination: u8, bytes_below_fp: i64) -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::SubImm,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(destination)),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(bytes_below_fp),
            ],
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
        MachineInst {
            opcode: ArmOpcode::Ret,
            operands: vec![],
            def: None,
        }
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

    fn ldp_i128_result() -> MachineInst {
        MachineInst {
            opcode: ArmOpcode::LdpOffset,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(-16),
            ],
            def: None,
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
        let mut mf = build_mf(vec![vec![bl("_foo"), ldp_post(), ret()]]);
        tail_call_opt(&mut mf);
        let insts = &mf.blocks[0].insts;
        // Should now be: LdpPost, B _foo
        assert_eq!(insts.len(), 2);
        assert_eq!(insts[0].opcode, ArmOpcode::LdpPost);
        assert_eq!(insts[1].opcode, ArmOpcode::B);
        assert_eq!(insts[1].operands[0], MachineOperand::Extern("_foo".into()));
    }

    #[test]
    fn void_tail_call_with_callee_restore() {
        // Pattern: Bl; LdrImm(restore); LdpPost; Ret → LdrImm; LdpPost; B
        let mut mf = build_mf(vec![vec![
            bl("_bar"),
            ldr_callee_restore(),
            ldp_post(),
            ret(),
        ]]);
        mf.callee_save_slots.push((PhysReg::Gp(19), -8));
        tail_call_opt(&mut mf);
        let insts = &mf.blocks[0].insts;
        assert_eq!(insts.len(), 3);
        assert_eq!(insts[0].opcode, ArmOpcode::LdrImm);
        assert_eq!(insts[1].opcode, ArmOpcode::LdpPost);
        assert_eq!(insts[2].opcode, ArmOpcode::B);
    }

    #[test]
    fn no_tco_when_i128_result_is_loaded_after_call() {
        let mut mf = build_mf(vec![vec![
            bl("_side"),
            ldp_i128_result(),
            ldp_post(),
            ret(),
        ]]);

        tail_call_opt(&mut mf);

        let insts = &mf.blocks[0].insts;
        assert!(insts.iter().any(|inst| inst.opcode == ArmOpcode::Bl));
        assert!(insts.iter().any(|inst| inst.opcode == ArmOpcode::Ret));
        assert!(!insts.iter().any(|inst| inst.opcode == ArmOpcode::B));
    }

    #[test]
    fn no_tco_when_call_uses_outgoing_stack_arguments() {
        let mut mf = build_mf(vec![vec![
            bl_with_stack_args("_sink", 8),
            ldp_post(),
            ret(),
        ]]);

        tail_call_opt(&mut mf);

        let insts = &mf.blocks[0].insts;
        assert!(insts.iter().any(|inst| inst.opcode == ArmOpcode::Bl));
        assert!(insts.iter().any(|inst| inst.opcode == ArmOpcode::Ret));
        assert!(!insts.iter().any(|inst| inst.opcode == ArmOpcode::B));
    }

    #[test]
    fn register_only_tail_call_survives_unrelated_outgoing_frame_space() {
        let mut mf = build_mf(vec![vec![bl("_sink"), ldp_post(), ret()]]);
        mf.reserve_outgoing_args(16);

        tail_call_opt(&mut mf);

        let insts = &mf.blocks[0].insts;
        assert!(!insts.iter().any(|inst| inst.opcode == ArmOpcode::Bl));
        assert!(!insts.iter().any(|inst| inst.opcode == ArmOpcode::Ret));
        assert!(insts.iter().any(|inst| inst.opcode == ArmOpcode::B));
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
        let mut mf = build_mf(vec![vec![bl("_baz"), mov_result, ldp_post(), ret()]]);
        tail_call_opt(&mut mf);
        let insts = &mf.blocks[0].insts;
        // No transformation — Bl should still be present.
        assert!(
            insts.iter().any(|i| i.opcode == ArmOpcode::Bl),
            "Bl should NOT be removed when result is captured in non-return register"
        );
        assert!(
            insts.iter().any(|i| i.opcode == ArmOpcode::Ret),
            "Ret should still be present"
        );
    }

    #[test]
    fn no_tco_when_not_ending_in_ret() {
        // Block ending in B (not Ret) — no TCO.
        let b_inst = MachineInst {
            opcode: ArmOpcode::B,
            operands: vec![MachineOperand::BlockRef(MBlockId(1))],
            def: None,
        };
        let mut mf = build_mf(vec![vec![bl("_qux"), ldp_post(), b_inst]]);
        tail_call_opt(&mut mf);
        // Should still have Bl (TCO not fired because no Ret).
        assert!(mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::Bl));
    }

    #[test]
    fn no_tco_when_frame_pointer_is_spilled_through_large_offset_slot() {
        // Repro shape from fortbite O1:
        //   sub x10, fp, #104
        //   sub x8,  fp, #1936
        //   str x10, [x8]
        //   sub x8,  fp, #1936
        //   ldr x22, [x8]
        //   mov x1,  x22
        //   bl _callee
        //
        // The arg in x1 is a pointer into our frame, just spilled/reloaded
        // through the large-offset materialization form. Tail-branching here
        // would tear down the frame before the callee consumes x1.
        let mut mf = build_mf(vec![vec![
            MachineInst {
                opcode: ArmOpcode::SubImm,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(10)),
                    MachineOperand::PhysReg(PhysReg::FP),
                    MachineOperand::Imm(104),
                ],
                def: None,
            },
            MachineInst {
                opcode: ArmOpcode::SubImm,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(8)),
                    MachineOperand::PhysReg(PhysReg::FP),
                    MachineOperand::Imm(1936),
                ],
                def: None,
            },
            MachineInst {
                opcode: ArmOpcode::StrImm,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(10)),
                    MachineOperand::PhysReg(PhysReg::Gp(8)),
                    MachineOperand::Imm(0),
                ],
                def: None,
            },
            MachineInst {
                opcode: ArmOpcode::SubImm,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(8)),
                    MachineOperand::PhysReg(PhysReg::FP),
                    MachineOperand::Imm(1936),
                ],
                def: None,
            },
            MachineInst {
                opcode: ArmOpcode::LdrImm,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(22)),
                    MachineOperand::PhysReg(PhysReg::Gp(8)),
                    MachineOperand::Imm(0),
                ],
                def: None,
            },
            MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(1)),
                    MachineOperand::PhysReg(PhysReg::Gp(22)),
                ],
                def: None,
            },
            bl("_callee"),
            ldp_post(),
            ret(),
        ]]);
        tail_call_opt(&mut mf);
        assert!(
            mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::Bl),
            "tail-call optimization must not erase the call when an arg reloads a spilled frame pointer"
        );
        assert!(
            mf.blocks[0]
                .insts
                .iter()
                .any(|i| i.opcode == ArmOpcode::Ret),
            "tail-call optimization must leave the normal return path intact"
        );
    }

    #[test]
    fn no_tco_when_arg_register_is_reloaded_from_frame_in_tail_block() {
        let mut mf = build_mf(vec![
            vec![
                MachineInst {
                    opcode: ArmOpcode::SubImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(10)),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(104),
                    ],
                    def: None,
                },
                MachineInst {
                    opcode: ArmOpcode::StrImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(10)),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(-1936),
                    ],
                    def: None,
                },
                MachineInst {
                    opcode: ArmOpcode::B,
                    operands: vec![MachineOperand::BlockRef(MBlockId(1))],
                    def: None,
                },
            ],
            vec![
                MachineInst {
                    opcode: ArmOpcode::LdrImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(1)),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(-1936),
                    ],
                    def: None,
                },
                bl("_callee"),
                ldp_post(),
                ret(),
            ],
        ]);
        tail_call_opt(&mut mf);
        assert!(
            mf.blocks[1].insts.iter().any(|i| i.opcode == ArmOpcode::Bl),
            "tail-call optimization must not fire when x1 is reloaded from our frame in the tail block"
        );
        assert!(
            mf.blocks[1].insts.iter().any(|i| i.opcode == ArmOpcode::Ret),
            "tail-call optimization must preserve the return when the arg comes from a frame reload"
        );
    }

    #[test]
    fn no_tco_when_argument_is_frame_derived_in_predecessor() {
        let mut mf = build_mf(vec![
            vec![frame_address(0, 32), branch(1)],
            vec![bl("_callee"), ldp_post(), ret()],
        ]);

        tail_call_opt(&mut mf);

        assert!(
            mf.blocks[1]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Bl),
            "tail-call optimization must preserve a call fed by a frame-derived predecessor register"
        );
        assert!(
            mf.blocks[1]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Ret),
            "the successor must retain its normal return while x0 points into the caller frame"
        );
    }

    #[test]
    fn no_tco_when_frame_derived_argument_reaches_a_join_from_one_predecessor() {
        let mut mf = build_mf(vec![
            vec![conditional_branch(1), branch(2)],
            vec![frame_address(0, 32), branch(3)],
            vec![branch(3)],
            vec![bl("_callee"), ldp_post(), ret()],
        ]);

        tail_call_opt(&mut mf);

        assert!(
            mf.blocks[3]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Bl),
            "taint from either predecessor must block frame teardown at the join"
        );
        assert!(
            mf.blocks[3]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Ret),
            "the joined tail block must retain its normal return"
        );
    }

    #[test]
    fn no_tco_when_joined_frame_bases_have_different_offsets() {
        let mut mf = build_mf(vec![
            vec![conditional_branch(1), branch(2)],
            vec![frame_address(10, 32), branch(3)],
            vec![frame_address(10, 64), branch(3)],
            vec![
                MachineInst {
                    opcode: ArmOpcode::LdrImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(0)),
                        MachineOperand::PhysReg(PhysReg::Gp(10)),
                        MachineOperand::Imm(0),
                    ],
                    def: None,
                },
                bl("_callee"),
                ldp_post(),
                ret(),
            ],
        ]);

        tail_call_opt(&mut mf);

        assert!(
            mf.blocks[3]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Bl),
            "conflicting predecessor offsets must remain a conservative frame address"
        );
        assert!(
            mf.blocks[3]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Ret),
            "a load through a joined frame base must preserve the caller frame"
        );
    }

    #[test]
    fn no_tco_when_frame_taint_arrives_over_a_backedge() {
        let mut mf = build_mf(vec![
            vec![branch(1)],
            vec![conditional_branch(2), bl("_callee"), ldp_post(), ret()],
            vec![frame_address(0, 32), branch(1)],
        ]);

        tail_call_opt(&mut mf);

        assert!(
            mf.blocks[1]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Bl),
            "fixed-point analysis must include a frame-derived argument carried by a backedge"
        );
        assert!(
            mf.blocks[1]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Ret),
            "a loop-carried local address must retain the normal return path"
        );
    }

    #[test]
    fn no_tco_when_frame_taint_reaches_the_next_block_by_fallthrough() {
        let mut mf = build_mf(vec![
            vec![frame_address(0, 32)],
            vec![bl("_callee"), ldp_post(), ret()],
        ]);

        tail_call_opt(&mut mf);

        assert!(
            mf.blocks[1]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Bl),
            "layout fallthrough must carry frame-derived argument state"
        );
    }

    #[test]
    fn clean_cross_block_tail_call_remains_eligible() {
        let mut mf = build_mf(vec![
            vec![branch(1)],
            vec![bl("_callee"), ldp_post(), ret()],
        ]);

        tail_call_opt(&mut mf);

        assert!(
            mf.blocks[1]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::B),
            "a clean predecessor must not suppress a safe tail call"
        );
        assert!(
            !mf.blocks[1]
                .insts
                .iter()
                .any(|inst| inst.opcode == ArmOpcode::Bl || inst.opcode == ArmOpcode::Ret),
            "the safe cross-block case should still remove the call and return"
        );
    }

    #[test]
    fn no_tco_when_temp_register_reloads_frame_slot_before_copying_to_arg() {
        let mut mf = build_mf(vec![
            vec![
                MachineInst {
                    opcode: ArmOpcode::SubImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(10)),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(104),
                    ],
                    def: None,
                },
                MachineInst {
                    opcode: ArmOpcode::StrImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(10)),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(-1936),
                    ],
                    def: None,
                },
                MachineInst {
                    opcode: ArmOpcode::B,
                    operands: vec![MachineOperand::BlockRef(MBlockId(1))],
                    def: None,
                },
            ],
            vec![
                MachineInst {
                    opcode: ArmOpcode::LdrImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(22)),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(-1936),
                    ],
                    def: None,
                },
                MachineInst {
                    opcode: ArmOpcode::MovReg,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(1)),
                        MachineOperand::PhysReg(PhysReg::Gp(22)),
                    ],
                    def: None,
                },
                bl("_callee"),
                ldp_post(),
                ret(),
            ],
        ]);
        tail_call_opt(&mut mf);
        assert!(
            mf.blocks[1].insts.iter().any(|i| i.opcode == ArmOpcode::Bl),
            "tail-call optimization must not fire when a temp reloads a frame slot before copying it to x1"
        );
        assert!(
            mf.blocks[1].insts.iter().any(|i| i.opcode == ArmOpcode::Ret),
            "tail-call optimization must preserve the return when a frame reload feeds x1 indirectly"
        );
    }

    #[test]
    fn no_tco_when_csel_can_select_frame_derived_pointer_into_argument() {
        for (true_value, false_value) in [(10, 11), (11, 10)] {
            let mut mf = build_mf(vec![vec![
                MachineInst {
                    opcode: ArmOpcode::SubImm,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(10)),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(32),
                    ],
                    def: None,
                },
                MachineInst {
                    opcode: ArmOpcode::CselReg,
                    operands: vec![
                        MachineOperand::PhysReg(PhysReg::Gp(0)),
                        MachineOperand::PhysReg(PhysReg::Gp(true_value)),
                        MachineOperand::PhysReg(PhysReg::Gp(false_value)),
                        MachineOperand::Cond(ArmCond::Ne),
                    ],
                    def: None,
                },
                bl("_callee"),
                ldp_post(),
                ret(),
            ]]);

            tail_call_opt(&mut mf);

            assert!(
                mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::Bl),
                "tail-call optimization must not tear down a frame when CSEL can choose x{true_value} or x{false_value} into x0"
            );
            assert!(
                mf.blocks[0]
                    .insts
                    .iter()
                    .any(|i| i.opcode == ArmOpcode::Ret),
                "the normal return must remain when CSEL can choose x{true_value} or x{false_value} into x0"
            );
        }
    }

    #[test]
    fn csel_overwrite_invalidates_exact_frame_address_metadata() {
        let mut mf = build_mf(vec![vec![
            MachineInst {
                opcode: ArmOpcode::SubImm,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(10)),
                    MachineOperand::PhysReg(PhysReg::FP),
                    MachineOperand::Imm(32),
                ],
                def: None,
            },
            MachineInst {
                opcode: ArmOpcode::CselReg,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(10)),
                    MachineOperand::PhysReg(PhysReg::Gp(11)),
                    MachineOperand::PhysReg(PhysReg::Gp(12)),
                    MachineOperand::Cond(ArmCond::Ne),
                ],
                def: None,
            },
            MachineInst {
                opcode: ArmOpcode::LdrImm,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(0)),
                    MachineOperand::PhysReg(PhysReg::Gp(10)),
                    MachineOperand::Imm(0),
                ],
                def: None,
            },
            bl("_callee"),
            ldp_post(),
            ret(),
        ]]);

        tail_call_opt(&mut mf);

        assert!(
            mf.blocks[0].insts.iter().any(|i| i.opcode == ArmOpcode::B),
            "a CSEL overwrite must discard stale exact-frame-address metadata"
        );
        assert!(
            !mf.blocks[0]
                .insts
                .iter()
                .any(|i| i.opcode == ArmOpcode::Ret),
            "clean selected bases must not spuriously block a safe tail call"
        );
    }
}
