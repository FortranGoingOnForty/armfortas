//! Liveness analysis for Machine IR.
//!
//! Computes live intervals for each virtual register using backward
//! dataflow. Used by the linear scan register allocator.

use super::mir::*;
use std::collections::HashMap;

/// A live interval: the range of instruction positions where a vreg is live.
#[derive(Debug, Clone)]
pub struct LiveInterval {
    pub vreg: VRegId,
    pub class: RegClass,
    pub start: u32, // first instruction position (definition)
    pub end: u32,   // last instruction position (final use)
    /// Preferred physical register (hint). If set, the allocator tries this register first.
    /// Used to avoid unnecessary moves (e.g., arg values prefer xN, return values prefer x0).
    pub hint: Option<u8>,
}

/// Result of liveness analysis.
pub struct LivenessResult {
    pub intervals: Vec<LiveInterval>,
    /// Sorted BL/BLR positions, stored once for lazy interval queries.
    pub call_positions: Vec<u32>,
    /// Total number of instruction positions.
    pub num_positions: u32,
}

#[derive(Clone, PartialEq, Eq)]
struct LiveSet {
    words: Vec<u64>,
}

impl LiveSet {
    fn new(num_vregs: usize) -> Self {
        let num_words = num_vregs.div_ceil(64);
        Self {
            words: vec![0; num_words],
        }
    }

    fn clear(&mut self) {
        self.words.fill(0);
    }

    fn copy_from(&mut self, other: &Self) {
        self.words.copy_from_slice(&other.words);
    }

    fn insert(&mut self, vreg: VRegId) {
        let idx = vreg.0 as usize;
        let word = idx / 64;
        let bit = idx % 64;
        if let Some(slot) = self.words.get_mut(word) {
            *slot |= 1u64 << bit;
        }
    }

    fn remove(&mut self, vreg: &VRegId) {
        let idx = vreg.0 as usize;
        let word = idx / 64;
        let bit = idx % 64;
        if let Some(slot) = self.words.get_mut(word) {
            *slot &= !(1u64 << bit);
        }
    }

    fn union_with(&mut self, other: &Self) {
        for (dst, src) in self.words.iter_mut().zip(&other.words) {
            *dst |= *src;
        }
    }

    fn iter(&self) -> LiveSetIter<'_> {
        LiveSetIter {
            words: &self.words,
            word_idx: 0,
            active_word: self.words.first().copied().unwrap_or(0),
        }
    }
}

struct LiveSetIter<'a> {
    words: &'a [u64],
    word_idx: usize,
    active_word: u64,
}

impl Iterator for LiveSetIter<'_> {
    type Item = VRegId;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.active_word != 0 {
                let bit = self.active_word.trailing_zeros() as usize;
                self.active_word &= self.active_word - 1;
                let idx = self.word_idx * 64 + bit;
                return Some(VRegId(idx as u32));
            }

            self.word_idx += 1;
            if self.word_idx >= self.words.len() {
                return None;
            }
            self.active_word = self.words[self.word_idx];
        }
    }
}

fn machine_vreg_capacity(mf: &MachineFunction) -> usize {
    let mut max_idx = 0usize;
    for vreg in &mf.vregs {
        max_idx = max_idx.max(vreg.id.0 as usize + 1);
    }
    for block in &mf.blocks {
        for inst in &block.insts {
            if let Some(def) = inst.def {
                max_idx = max_idx.max(def.0 as usize + 1);
            }
            for op in &inst.operands {
                if let MachineOperand::VReg(vreg) = op {
                    max_idx = max_idx.max(vreg.0 as usize + 1);
                }
            }
        }
    }
    max_idx
}

/// Compute live intervals for all vregs in a machine function.
pub fn compute_liveness(mf: &MachineFunction) -> LivenessResult {
    let num_vregs = machine_vreg_capacity(mf);
    let block_indices: HashMap<MBlockId, usize> = mf
        .blocks
        .iter()
        .enumerate()
        .map(|(idx, block)| (block.id, idx))
        .collect();

    // Phase 1: assign a linear position to each instruction.
    // Positions increase by 2 (leave gaps for inserted moves).
    let mut inst_positions: Vec<Vec<u32>> = Vec::with_capacity(mf.blocks.len());
    let mut block_start: Vec<u32> = Vec::with_capacity(mf.blocks.len());
    let mut block_end: Vec<u32> = Vec::with_capacity(mf.blocks.len());
    let mut pos: u32 = 0;

    for block in &mf.blocks {
        block_start.push(pos);
        let mut block_positions = Vec::with_capacity(block.insts.len());
        for _inst in &block.insts {
            block_positions.push(pos);
            pos += 2;
        }
        inst_positions.push(block_positions);
        block_end.push(pos);
    }
    let num_positions = pos;

    // Phase 3: backward dataflow to compute live-in and live-out per block.
    let mut live_in: Vec<LiveSet> = (0..mf.blocks.len())
        .map(|_| LiveSet::new(num_vregs))
        .collect();
    let mut live_out: Vec<LiveSet> = (0..mf.blocks.len())
        .map(|_| LiveSet::new(num_vregs))
        .collect();

    // Build successor map from branch targets.
    // Our codegen emits conditional branches as: CmpImm, BCond, B (unconditional fallback).
    // So a block can have both BCond and B targets.
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); mf.blocks.len()];
    for (block_idx, block) in mf.blocks.iter().enumerate() {
        let mut succs = Vec::new();
        // Scan ALL instructions in the block for branch targets (not just the last).
        for inst in &block.insts {
            match inst.opcode {
                ArmOpcode::B => {
                    if let Some(MachineOperand::BlockRef(target)) = inst.operands.first() {
                        if let Some(&succ_idx) = block_indices.get(target) {
                            if !succs.contains(&succ_idx) {
                                succs.push(succ_idx);
                            }
                        }
                    }
                }
                ArmOpcode::BCond => {
                    if let Some(MachineOperand::BlockRef(target)) = inst.operands.get(1) {
                        if let Some(&succ_idx) = block_indices.get(target) {
                            if !succs.contains(&succ_idx) {
                                succs.push(succ_idx);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        successors[block_idx] = succs;
    }

    // Iterate until fixed point.
    let mut changed = true;
    let mut scratch_out = LiveSet::new(num_vregs);
    let mut scratch_in = LiveSet::new(num_vregs);
    while changed {
        changed = false;
        for block_idx in (0..mf.blocks.len()).rev() {
            let block = &mf.blocks[block_idx];
            // live_out = union of live_in of all successors.
            scratch_out.clear();
            for &succ_idx in &successors[block_idx] {
                scratch_out.union_with(&live_in[succ_idx]);
            }

            // live_in = (live_out - defs) ∪ uses
            scratch_in.copy_from(&scratch_out);
            // Walk instructions backward.
            for inst in block.insts.iter().rev() {
                // Remove defs.
                if let Some(def) = &inst.def {
                    scratch_in.remove(def);
                }
                // Add uses.
                for op in &inst.operands {
                    if let MachineOperand::VReg(vid) = op {
                        scratch_in.insert(*vid);
                    }
                }
            }

            if live_in[block_idx] != scratch_in {
                live_in[block_idx].copy_from(&scratch_in);
                changed = true;
            }
            if live_out[block_idx] != scratch_out {
                live_out[block_idx].copy_from(&scratch_out);
                changed = true;
            }
        }
    }

    // Phase 4: build live intervals from def/use positions.
    let mut starts: Vec<Option<u32>> = vec![None; num_vregs];
    let mut ends: Vec<Option<u32>> = vec![None; num_vregs];

    for (block_idx, block) in mf.blocks.iter().enumerate() {
        let b_start = block_start[block_idx];
        let b_end = block_end[block_idx];

        // Vregs live-in to this block: extend their interval to block start.
        for vreg in live_in[block_idx].iter() {
            let idx = vreg.0 as usize;
            ends[idx] = Some(ends[idx].map_or(b_end, |end| end.max(b_end)));
            starts[idx] = Some(starts[idx].map_or(b_start, |start| start.min(b_start)));
        }

        // Vregs live-out of this block: extend their interval to block end.
        for vreg in live_out[block_idx].iter() {
            let idx = vreg.0 as usize;
            ends[idx] = Some(ends[idx].map_or(b_end, |end| end.max(b_end)));
            starts[idx] = Some(starts[idx].map_or(b_start, |start| start.min(b_start)));
        }

        // Walk instructions for precise def/use positions.
        for (i, inst) in block.insts.iter().enumerate() {
            let p = inst_positions[block_idx][i];
            if let Some(def) = &inst.def {
                let idx = def.0 as usize;
                starts[idx] = Some(starts[idx].map_or(p, |start| start.min(p)));
                ends[idx] = Some(ends[idx].map_or(p, |end| end.max(p)));
            }
            for op in &inst.operands {
                if let MachineOperand::VReg(vid) = op {
                    let idx = vid.0 as usize;
                    ends[idx] = Some(ends[idx].map_or(p, |end| end.max(p)));
                    starts[idx] = Some(starts[idx].map_or(p, |start| start.min(p)));
                }
            }
        }
    }

    // Collect positions of all call instructions. Both direct
    // (`Bl`) and indirect (`Blr`) calls clobber every caller-saved
    // register and must be visible to the allocator's lazy crossing query.
    let mut call_positions: Vec<u32> = Vec::new();
    for (block_idx, block) in mf.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            if matches!(inst.opcode, ArmOpcode::Bl | ArmOpcode::Blr) {
                call_positions.push(inst_positions[block_idx][i]);
            }
        }
    }
    call_positions.sort_unstable();

    // Build intervals.
    let mut vreg_classes = vec![RegClass::Gp64; num_vregs];
    for vreg in &mf.vregs {
        vreg_classes[vreg.id.0 as usize] = vreg.class;
    }

    let mut intervals: Vec<LiveInterval> = starts
        .iter()
        .enumerate()
        .filter_map(|(idx, start)| {
            let start = (*start)?;
            let end = ends[idx]?;
            let vreg = VRegId(idx as u32);
            let class = vreg_classes[idx];
            Some(LiveInterval {
                vreg,
                class,
                start,
                end,
                hint: None,
            })
        })
        .collect();

    // Sort by start position, breaking ties on the vreg ID. The
    // tie-break is critical: linear-scan register allocation
    // processes intervals in vector order, and `starts` is a
    // HashMap whose iteration order varies between runs. Without
    // a deterministic tie-break, two compiles of the same source
    // could pick different registers and produce different
    // assembly — surfaced as the long-standing select_type.f90
    // intermittent failure during the optimizer audit rounds and
    // exposed clearly when mem2reg started perturbing vreg counts.
    intervals.sort_by_key(|i| (i.start, i.vreg.0));

    LivenessResult {
        intervals,
        call_positions,
        num_positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::isel::select_function;
    use crate::ir::builder::FuncBuilder;
    use crate::ir::inst::*;
    use crate::ir::types::*;

    #[test]
    fn liveness_simple_function() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mf = select_function(&func, crate::target::TargetLayout::LP64);
        let result = compute_liveness(&mf);
        assert!(!result.intervals.is_empty(), "should have live intervals");
        assert!(result.num_positions > 0);
    }

    #[test]
    fn liveness_intervals_have_correct_class() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let a = b.const_f64(std::f64::consts::PI);
            let c = b.const_f64(2.0);
            let _d = b.fadd(a, c);
            b.ret_void();
        }
        let mf = select_function(&func, crate::target::TargetLayout::LP64);
        let result = compute_liveness(&mf);
        // Should have some Fp64 intervals.
        assert!(
            result.intervals.iter().any(|i| i.class == RegClass::Fp64),
            "should have Fp64 intervals"
        );
    }

    #[test]
    fn intervals_crossing_blr_are_marked_call_crossing() {
        // Construct: def vreg %0 → BLR %0 → use %0. Since %0 is
        // both the BLR target and a value live across the call,
        // its interval must report the crossing so the allocator
        // pins it to a callee-saved register (or
        // accepts the spill rather than burying the value in a
        // caller-saved that the BLR will clobber).
        let mut mf = MachineFunction::new("test".into());
        let v0 = mf.new_vreg(RegClass::Gp64);
        let v1 = mf.new_vreg(RegClass::Gp64);
        // Define %0.
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Movz,
            operands: vec![MachineOperand::VReg(v0), MachineOperand::Imm(42)],
            def: Some(v0),
        });
        // BLR through %0 (an indirect call).
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::VReg(v0)],
            def: None,
        });
        // Use %0 again post-call (forces the interval to cross
        // the BLR position).
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![MachineOperand::VReg(v1), MachineOperand::VReg(v0)],
            def: Some(v1),
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Ret,
            operands: vec![],
            def: None,
        });
        let result = compute_liveness(&mf);
        let interval = result
            .intervals
            .iter()
            .find(|i| i.vreg == v0)
            .expect("vreg 0 must have an interval");
        assert_eq!(
            crate::codegen::shared::classify_call_crossing(
                &result.call_positions,
                interval.start,
                interval.end,
            ),
            crate::codegen::shared::CallCrossing::One(2),
            "interval should lazily resolve its single BLR crossing"
        );
    }

    #[test]
    fn call_positions_include_direct_and_indirect_calls_once() {
        let mut mf = MachineFunction::new("calls".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![MachineOperand::Extern("callee".into())],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::PhysReg(PhysReg::Gp(9))],
            def: None,
        });

        let result = compute_liveness(&mf);
        assert_eq!(result.call_positions, vec![0, 2]);
    }

    #[test]
    fn call_crossing_storage_scales_with_calls_and_intervals() {
        const COUNT: usize = 128;
        assert!(
            !std::mem::needs_drop::<LiveInterval>(),
            "live intervals must not own per-interval call metadata"
        );

        let mut mf = MachineFunction::new("call_matrix".into());
        let mut long_lived = Vec::with_capacity(COUNT);
        for value in 0..COUNT {
            let vreg = mf.new_vreg(RegClass::Gp64);
            long_lived.push(vreg);
            mf.blocks[0].insts.push(MachineInst {
                opcode: ArmOpcode::Movz,
                operands: vec![
                    MachineOperand::VReg(vreg),
                    MachineOperand::Imm(value as i64),
                ],
                def: Some(vreg),
            });
        }
        for call in 0..COUNT {
            let (opcode, operand) = if call % 2 == 0 {
                (ArmOpcode::Bl, MachineOperand::Extern("callee".into()))
            } else {
                (ArmOpcode::Blr, MachineOperand::PhysReg(PhysReg::Gp(9)))
            };
            mf.blocks[0].insts.push(MachineInst {
                opcode,
                operands: vec![operand],
                def: None,
            });
        }
        for &source in &long_lived {
            let result = mf.new_vreg(RegClass::Gp64);
            mf.blocks[0].insts.push(MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![MachineOperand::VReg(result), MachineOperand::VReg(source)],
                def: Some(result),
            });
        }

        let liveness = compute_liveness(&mf);
        assert_eq!(liveness.call_positions.len(), COUNT);
        for vreg in long_lived {
            let interval = liveness
                .intervals
                .iter()
                .find(|interval| interval.vreg == vreg)
                .expect("long-lived interval");
            assert_eq!(
                crate::codegen::shared::classify_call_crossing(
                    &liveness.call_positions,
                    interval.start,
                    interval.end,
                ),
                crate::codegen::shared::CallCrossing::Multiple
            );
        }
    }

    #[test]
    fn liveness_branching() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let cond = b.const_bool(true);
            let bb_t = b.create_block("then");
            let bb_f = b.create_block("else");
            b.cond_branch(cond, bb_t, vec![], bb_f, vec![]);
            b.set_block(bb_t);
            b.ret_void();
            b.set_block(bb_f);
            b.ret_void();
        }
        let mf = select_function(&func, crate::target::TargetLayout::LP64);
        let result = compute_liveness(&mf);
        assert!(result.num_positions > 0);
    }
}
