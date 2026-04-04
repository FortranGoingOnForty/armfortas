//! Liveness analysis for Machine IR.
//!
//! Computes live intervals for each virtual register using backward
//! dataflow. Used by the linear scan register allocator.

use std::collections::{HashMap, HashSet, BTreeSet};
use super::mir::*;

/// A live interval: the range of instruction positions where a vreg is live.
#[derive(Debug, Clone)]
pub struct LiveInterval {
    pub vreg: VRegId,
    pub class: RegClass,
    pub start: u32,   // first instruction position (definition)
    pub end: u32,     // last instruction position (final use)
    pub crosses_call: bool, // true if a BL instruction falls within [start, end]
}

/// Result of liveness analysis.
pub struct LivenessResult {
    pub intervals: Vec<LiveInterval>,
    /// Total number of instruction positions.
    pub num_positions: u32,
}

/// Compute live intervals for all vregs in a machine function.
pub fn compute_liveness(mf: &MachineFunction) -> LivenessResult {
    // Phase 1: assign a linear position to each instruction.
    // Positions increase by 2 (leave gaps for inserted moves).
    let mut inst_positions: Vec<(MBlockId, usize, u32)> = Vec::new(); // (block, inst_idx, position)
    let mut block_start: HashMap<MBlockId, u32> = HashMap::new();
    let mut block_end: HashMap<MBlockId, u32> = HashMap::new();
    let mut pos: u32 = 0;

    for block in &mf.blocks {
        block_start.insert(block.id, pos);
        for (i, _inst) in block.insts.iter().enumerate() {
            inst_positions.push((block.id, i, pos));
            pos += 2;
        }
        block_end.insert(block.id, pos);
    }
    let num_positions = pos;

    // Phase 2: compute uses and defs per block.
    // Also build a map from (block, inst_idx) → position.
    let mut position_map: HashMap<(MBlockId, usize), u32> = HashMap::new();
    for &(bid, idx, p) in &inst_positions {
        position_map.insert((bid, idx), p);
    }

    // Phase 3: backward dataflow to compute live-in and live-out per block.
    let mut live_in: HashMap<MBlockId, BTreeSet<VRegId>> = HashMap::new();
    let mut live_out: HashMap<MBlockId, BTreeSet<VRegId>> = HashMap::new();
    for block in &mf.blocks {
        live_in.insert(block.id, BTreeSet::new());
        live_out.insert(block.id, BTreeSet::new());
    }

    // Build successor map from branch targets.
    let mut successors: HashMap<MBlockId, Vec<MBlockId>> = HashMap::new();
    for block in &mf.blocks {
        let mut succs = Vec::new();
        if let Some(last) = block.insts.last() {
            match last.opcode {
                ArmOpcode::B => {
                    if let Some(MachineOperand::BlockRef(target)) = last.operands.first() {
                        succs.push(*target);
                    }
                }
                ArmOpcode::BCond => {
                    // B.cond is always followed by an unconditional B in our codegen.
                    // The B.cond target is operand[1], the B target is in the next inst.
                    if let Some(MachineOperand::BlockRef(target)) = last.operands.get(1) {
                        succs.push(*target);
                    }
                    // Check the second-to-last instruction for B.cond + B pattern.
                    if block.insts.len() >= 2 {
                        let prev = &block.insts[block.insts.len() - 2];
                        if prev.opcode == ArmOpcode::BCond {
                            if let Some(MachineOperand::BlockRef(target)) = prev.operands.get(1) {
                                succs.push(*target);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        successors.insert(block.id, succs);
    }

    // Iterate until fixed point.
    let mut changed = true;
    while changed {
        changed = false;
        for block in mf.blocks.iter().rev() {
            // live_out = union of live_in of all successors.
            let mut new_out = BTreeSet::new();
            if let Some(succs) = successors.get(&block.id) {
                for succ in succs {
                    if let Some(succ_in) = live_in.get(succ) {
                        new_out.extend(succ_in);
                    }
                }
            }

            // live_in = (live_out - defs) ∪ uses
            let mut new_in = new_out.clone();
            // Walk instructions backward.
            for inst in block.insts.iter().rev() {
                // Remove defs.
                if let Some(def) = &inst.def {
                    new_in.remove(def);
                }
                // Add uses.
                for op in &inst.operands {
                    if let MachineOperand::VReg(vid) = op {
                        new_in.insert(*vid);
                    }
                }
            }

            if live_in.get(&block.id) != Some(&new_in) {
                live_in.insert(block.id, new_in);
                changed = true;
            }
            if live_out.get(&block.id) != Some(&new_out) {
                live_out.insert(block.id, new_out);
                changed = true;
            }
        }
    }

    // Phase 4: build live intervals from def/use positions.
    let mut starts: HashMap<VRegId, u32> = HashMap::new();
    let mut ends: HashMap<VRegId, u32> = HashMap::new();

    for block in &mf.blocks {
        let b_start = block_start[&block.id];
        let b_end = block_end[&block.id];

        // Vregs live-in to this block: extend their interval to block start.
        if let Some(lin) = live_in.get(&block.id) {
            for &vreg in lin {
                ends.entry(vreg).and_modify(|e| *e = (*e).max(b_end)).or_insert(b_end);
                starts.entry(vreg).and_modify(|s| *s = (*s).min(b_start)).or_insert(b_start);
            }
        }

        // Vregs live-out of this block: extend their interval to block end.
        if let Some(lout) = live_out.get(&block.id) {
            for &vreg in lout {
                ends.entry(vreg).and_modify(|e| *e = (*e).max(b_end)).or_insert(b_end);
                starts.entry(vreg).and_modify(|s| *s = (*s).min(b_start)).or_insert(b_start);
            }
        }

        // Walk instructions for precise def/use positions.
        for (i, inst) in block.insts.iter().enumerate() {
            let p = position_map[&(block.id, i)];
            if let Some(def) = &inst.def {
                starts.entry(*def).and_modify(|s| *s = (*s).min(p)).or_insert(p);
                ends.entry(*def).and_modify(|e| *e = (*e).max(p)).or_insert(p);
            }
            for op in &inst.operands {
                if let MachineOperand::VReg(vid) = op {
                    ends.entry(*vid).and_modify(|e| *e = (*e).max(p)).or_insert(p);
                    starts.entry(*vid).and_modify(|s| *s = (*s).min(p)).or_insert(p);
                }
            }
        }
    }

    // Collect positions of all call instructions.
    let mut call_positions: Vec<u32> = Vec::new();
    for block in &mf.blocks {
        for (i, inst) in block.insts.iter().enumerate() {
            if inst.opcode == ArmOpcode::Bl {
                if let Some(&p) = position_map.get(&(block.id, i)) {
                    call_positions.push(p);
                }
            }
        }
    }
    call_positions.sort();

    // Build intervals.
    let vreg_classes: HashMap<VRegId, RegClass> = mf.vregs.iter()
        .map(|v| (v.id, v.class))
        .collect();

    let mut intervals: Vec<LiveInterval> = starts.iter()
        .filter_map(|(&vreg, &start)| {
            let end = ends.get(&vreg).copied()?;
            let class = vreg_classes.get(&vreg).copied().unwrap_or(RegClass::Gp64);
            // Check if any call falls within [start, end].
            let crosses_call = call_positions.iter().any(|&cp| cp > start && cp < end);
            Some(LiveInterval { vreg, class, start, end, crosses_call })
        })
        .collect();

    // Sort by start position.
    intervals.sort_by_key(|i| i.start);

    LivenessResult { intervals, num_positions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::*;
    use crate::ir::inst::*;
    use crate::ir::builder::FuncBuilder;
    use crate::codegen::isel::select_function;

    #[test]
    fn liveness_simple_function() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mf = select_function(&func);
        let result = compute_liveness(&mf);
        assert!(!result.intervals.is_empty(), "should have live intervals");
        assert!(result.num_positions > 0);
    }

    #[test]
    fn liveness_intervals_have_correct_class() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let a = b.const_f64(3.14);
            let c = b.const_f64(2.0);
            let _d = b.fadd(a, c);
            b.ret_void();
        }
        let mf = select_function(&func);
        let result = compute_liveness(&mf);
        // Should have some Fp64 intervals.
        assert!(result.intervals.iter().any(|i| i.class == RegClass::Fp64),
            "should have Fp64 intervals");
    }

    #[test]
    fn liveness_branching() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let cond = b.const_bool(true);
            let bb_t = b.create_block("then");
            let bb_f = b.create_block("else");
            b.cond_branch(cond, bb_t, vec![], bb_f, vec![]);
            b.set_block(bb_t);
            b.ret_void();
            b.set_block(bb_f);
            b.ret_void();
        }
        let mf = select_function(&func);
        let result = compute_liveness(&mf);
        assert!(result.num_positions > 0);
    }
}
