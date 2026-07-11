//! Liveness analysis for the x86_64 Machine IR.
//!
//! Backward dataflow computing live intervals per virtual register,
//! consumed by the x86 linear-scan allocator (x10a). The algorithm is
//! the arch-neutral port of `arm64::liveness`; the x86-specific parts
//! are the branch opcodes used to build the successor map (Jmp/Jcc),
//! the call opcodes that mark interval call-crossings (Call/CallReg),
//! and the fact that an `X86Operand` carries its register class inline
//! (there is no separate vreg table as on arm64).

use super::mir::*;
use crate::codegen::shared::{MBlockId, VRegId};
use std::collections::HashMap;

/// A live interval: the range of instruction positions where a vreg is
/// live. Positions increase by 2 so inserted moves get odd slots.
#[derive(Debug, Clone)]
pub struct LiveInterval {
    pub vreg: VRegId,
    pub class: X86RegClass,
    pub start: u32,
    pub end: u32,
    /// Preferred physical register: this vreg is moved to/from this
    /// register by isel (an arg-setup `mov vreg, %rdi`, a return value,
    /// a div dividend, …). Allocating the vreg here turns the move into
    /// a self-move that coalescing then drops. The allocator tries it
    /// first if free and fixed-interval-clear over the vreg's range.
    pub hint: Option<X86Reg>,
}

/// Result of liveness analysis.
pub struct LivenessResult {
    pub intervals: Vec<LiveInterval>,
    /// Sorted Call/CallReg positions, stored once for lazy interval queries.
    pub call_positions: Vec<u32>,
    pub num_positions: u32,
}

#[derive(Clone, PartialEq, Eq)]
struct LiveSet {
    words: Vec<u64>,
}

impl LiveSet {
    fn new(num_vregs: usize) -> Self {
        Self {
            words: vec![0; num_vregs.div_ceil(64)],
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
        if let Some(slot) = self.words.get_mut(idx / 64) {
            *slot |= 1u64 << (idx % 64);
        }
    }
    fn remove(&mut self, vreg: &VRegId) {
        let idx = vreg.0 as usize;
        if let Some(slot) = self.words.get_mut(idx / 64) {
            *slot &= !(1u64 << (idx % 64));
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
                return Some(VRegId((self.word_idx * 64 + bit) as u32));
            }
            self.word_idx += 1;
            if self.word_idx >= self.words.len() {
                return None;
            }
            self.active_word = self.words[self.word_idx];
        }
    }
}

fn vreg_capacity(f: &X86Function) -> usize {
    let mut max_idx = 0usize;
    for block in &f.blocks {
        for inst in &block.insts {
            if let Some(def) = inst.def_vreg() {
                max_idx = max_idx.max(def.id.0 as usize + 1);
            }
            inst.for_each_use_vreg(|v| {
                max_idx = max_idx.max(v.id.0 as usize + 1);
            });
        }
    }
    max_idx
}

/// Bytes the backward-dataflow live sets will allocate for `f`: two
/// bitsets (`live_in` + `live_out`) per block, each `num_vregs` bits.
/// Used by the codegen driver to reject a pathologically large function
/// with a clear error instead of letting the `O(blocks × vregs)`
/// allocation OOM-kill the process.
pub fn liveness_footprint_bytes(f: &X86Function) -> u64 {
    let words = (vreg_capacity(f) as u64).div_ceil(64);
    2 * f.blocks.len() as u64 * words * 8
}

/// Compute live intervals for all vregs in an x86 machine function.
pub fn compute_liveness(f: &X86Function) -> LivenessResult {
    let num_vregs = vreg_capacity(f);
    let block_indices: HashMap<MBlockId, usize> = f
        .blocks
        .iter()
        .enumerate()
        .map(|(idx, block)| (block.id, idx))
        .collect();

    // Phase 1: linear position per instruction, stepping by 2.
    let mut inst_positions: Vec<Vec<u32>> = Vec::with_capacity(f.blocks.len());
    let mut block_start: Vec<u32> = Vec::with_capacity(f.blocks.len());
    let mut block_end: Vec<u32> = Vec::with_capacity(f.blocks.len());
    let mut pos: u32 = 0;
    for block in &f.blocks {
        block_start.push(pos);
        let mut positions = Vec::with_capacity(block.insts.len());
        for _ in &block.insts {
            positions.push(pos);
            pos += 2;
        }
        inst_positions.push(positions);
        block_end.push(pos);
    }
    let num_positions = pos;

    // Phase 2: successor map. Jmp carries its BlockRef at operand 0,
    // Jcc at operand 1 (operand 0 is the condition) — the same shape
    // as arm64 B / BCond.
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); f.blocks.len()];
    for (block_idx, block) in f.blocks.iter().enumerate() {
        let mut succs = Vec::new();
        for inst in &block.insts {
            let target_op = match inst.opcode {
                X86Opcode::Jmp => inst.operands.first(),
                X86Opcode::Jcc => inst.operands.get(1),
                _ => None,
            };
            if let Some(X86Operand::BlockRef(target)) = target_op {
                if let Some(&succ_idx) = block_indices.get(target) {
                    if !succs.contains(&succ_idx) {
                        succs.push(succ_idx);
                    }
                }
            }
        }
        successors[block_idx] = succs;
    }

    // Phase 3: backward dataflow to fixed point.
    let mut live_in: Vec<LiveSet> = (0..f.blocks.len())
        .map(|_| LiveSet::new(num_vregs))
        .collect();
    let mut live_out: Vec<LiveSet> = (0..f.blocks.len())
        .map(|_| LiveSet::new(num_vregs))
        .collect();
    let mut scratch_out = LiveSet::new(num_vregs);
    let mut scratch_in = LiveSet::new(num_vregs);
    let mut changed = true;
    while changed {
        changed = false;
        for block_idx in (0..f.blocks.len()).rev() {
            let block = &f.blocks[block_idx];
            scratch_out.clear();
            for &succ_idx in &successors[block_idx] {
                scratch_out.union_with(&live_in[succ_idx]);
            }
            scratch_in.copy_from(&scratch_out);
            for inst in block.insts.iter().rev() {
                if let Some(def) = inst.def_vreg() {
                    scratch_in.remove(&def.id);
                }
                inst.for_each_use_vreg(|v| scratch_in.insert(v.id));
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

    // Phase 4: build intervals from def/use positions and block-level
    // live-in/out extension.
    let mut starts: Vec<Option<u32>> = vec![None; num_vregs];
    let mut ends: Vec<Option<u32>> = vec![None; num_vregs];
    let mut classes: Vec<X86RegClass> = vec![X86RegClass::Gp64; num_vregs];

    for (block_idx, block) in f.blocks.iter().enumerate() {
        let b_start = block_start[block_idx];
        let b_end = block_end[block_idx];
        for vreg in live_in[block_idx].iter() {
            let idx = vreg.0 as usize;
            ends[idx] = Some(ends[idx].map_or(b_end, |e| e.max(b_end)));
            starts[idx] = Some(starts[idx].map_or(b_start, |s| s.min(b_start)));
        }
        for vreg in live_out[block_idx].iter() {
            let idx = vreg.0 as usize;
            ends[idx] = Some(ends[idx].map_or(b_end, |e| e.max(b_end)));
            starts[idx] = Some(starts[idx].map_or(b_start, |s| s.min(b_start)));
        }
        for (i, inst) in block.insts.iter().enumerate() {
            let p = inst_positions[block_idx][i];
            if let Some(def) = inst.def_vreg() {
                let idx = def.id.0 as usize;
                starts[idx] = Some(starts[idx].map_or(p, |s| s.min(p)));
                ends[idx] = Some(ends[idx].map_or(p, |e| e.max(p)));
                classes[idx] = def.class;
            }
            inst.for_each_use_vreg(|v| {
                let idx = v.id.0 as usize;
                ends[idx] = Some(ends[idx].map_or(p, |e| e.max(p)));
                starts[idx] = Some(starts[idx].map_or(p, |s| s.min(p)));
                classes[idx] = v.class;
            });
        }
    }

    // Both direct and indirect calls must be visible to the allocator's
    // lazy crossing query because they clobber all caller-saved registers.
    let mut call_positions: Vec<u32> = Vec::new();
    for (block_idx, block) in f.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            if matches!(inst.opcode, X86Opcode::Call | X86Opcode::CallReg) {
                call_positions.push(inst_positions[block_idx][i]);
            }
        }
    }
    call_positions.sort_unstable();

    // Register hints: a register-to-register move between a vreg and a
    // physical register (isel's arg setup, return value, div dividend,
    // …) hints the vreg toward that physreg, so the allocator can place
    // it there and coalescing drops the resulting self-move. First
    // hint wins (an earlier move's preference is more likely the def
    // site). Move opcodes that carry a value verbatim: MovRR (GP) and
    // the scalar/packed FP moves.
    let mut hints: HashMap<VRegId, X86Reg> = HashMap::new();
    for block in &f.blocks {
        for inst in &block.insts {
            if inst.fp_store_addr_vreg().is_some() {
                continue;
            }
            if !matches!(
                inst.opcode,
                X86Opcode::MovRR | X86Opcode::Movss | X86Opcode::Movsd | X86Opcode::Movaps
            ) {
                continue;
            }
            let def = inst.def.as_ref();
            let src = inst.operands.first();
            // mov vreg -> %phys  (def phys, src vreg)
            if let (Some(X86Operand::Reg(p)), Some(X86Operand::VReg(v))) = (def, src) {
                hints.entry(v.id).or_insert(*p);
            }
            // mov %phys -> vreg  (def vreg, src phys)
            if let (Some(X86Operand::VReg(v)), Some(X86Operand::Reg(p))) = (def, src) {
                hints.entry(v.id).or_insert(*p);
            }
        }
    }

    let mut intervals: Vec<LiveInterval> = starts
        .iter()
        .enumerate()
        .filter_map(|(idx, start)| {
            let start = (*start)?;
            let end = ends[idx]?;
            Some(LiveInterval {
                vreg: VRegId(idx as u32),
                class: classes[idx],
                start,
                end,
                hint: hints.get(&VRegId(idx as u32)).copied(),
            })
        })
        .collect();

    // Deterministic order: by start, tie-broken on vreg id. Linear
    // scan walks this vector, so a stable order keeps assembly
    // reproducible across runs (the arm64 select_type.f90 lesson).
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
    use crate::codegen::x86::isel::select_function;
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
        let mf = select_function(&func, &[], crate::target::TargetLayout::LP64);
        let result = compute_liveness(&mf);
        assert!(!result.intervals.is_empty());
        assert!(result.num_positions > 0);
    }

    #[test]
    fn liveness_has_fp_class() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let a = b.const_f64(std::f64::consts::PI);
            let c = b.const_f64(2.0);
            let _d = b.fadd(a, c);
            b.ret_void();
        }
        let mf = select_function(&func, &[], crate::target::TargetLayout::LP64);
        let result = compute_liveness(&mf);
        assert!(
            result
                .intervals
                .iter()
                .any(|i| matches!(i.class, X86RegClass::Xmm | X86RegClass::Xmm128)),
            "should have an Xmm interval"
        );
    }

    #[test]
    fn fp_store_address_vreg_is_a_use() {
        let mut mf = X86Function::new("fp_store_addr".into());
        let addr = mf.new_vreg(X86RegClass::Gp64);
        let value = mf.new_vreg(X86RegClass::Xmm);
        mf.blocks[0].insts.push(X86Inst {
            opcode: X86Opcode::MovRI,
            size: OpSize::Q,
            operands: vec![X86Operand::Imm(16)],
            def: Some(X86Operand::VReg(addr)),
        });
        mf.blocks[0].insts.push(X86Inst {
            opcode: X86Opcode::Movsd,
            size: OpSize::Q,
            operands: vec![X86Operand::Reg(X86Reg::Xmm0)],
            def: Some(X86Operand::VReg(value)),
        });
        mf.blocks[0].insts.push(X86Inst {
            opcode: X86Opcode::Movsd,
            size: OpSize::Q,
            operands: vec![X86Operand::VReg(value)],
            def: Some(X86Operand::VReg(addr)),
        });

        let store = &mf.blocks[0].insts[2];
        assert_eq!(store.fp_store_addr_vreg(), Some(addr));
        assert_eq!(store.def_vreg(), None);
        let mut uses = Vec::new();
        store.for_each_use_vreg(|v| uses.push(v.id));
        assert!(uses.contains(&addr.id), "store address must be a use");
        assert!(uses.contains(&value.id), "store value must remain a use");

        let result = compute_liveness(&mf);
        let addr_interval = result
            .intervals
            .iter()
            .find(|i| i.vreg == addr.id)
            .expect("address interval");
        assert_eq!(addr_interval.class, X86RegClass::Gp64);
        assert_eq!(
            addr_interval.hint, None,
            "store address must not inherit an FP move hint"
        );
    }

    #[test]
    fn call_positions_include_direct_and_indirect_calls_once() {
        let mut mf = X86Function::new("calls".into());
        mf.blocks[0].insts.push(X86Inst {
            opcode: X86Opcode::Call,
            size: OpSize::Q,
            operands: vec![X86Operand::Extern("callee".into())],
            def: None,
        });
        mf.blocks[0].insts.push(X86Inst {
            opcode: X86Opcode::CallReg,
            size: OpSize::Q,
            operands: vec![X86Operand::Reg(X86Reg::R11)],
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

        let mut mf = X86Function::new("call_matrix".into());
        let mut long_lived = Vec::with_capacity(COUNT);
        for value in 0..COUNT {
            let vreg = mf.new_vreg(X86RegClass::Gp64);
            long_lived.push(vreg);
            mf.blocks[0].insts.push(X86Inst {
                opcode: X86Opcode::MovRI,
                size: OpSize::Q,
                operands: vec![X86Operand::Imm(value as i64)],
                def: Some(X86Operand::VReg(vreg)),
            });
        }
        for call in 0..COUNT {
            let (opcode, operand) = if call % 2 == 0 {
                (X86Opcode::Call, X86Operand::Extern("callee".into()))
            } else {
                (X86Opcode::CallReg, X86Operand::Reg(X86Reg::R11))
            };
            mf.blocks[0].insts.push(X86Inst {
                opcode,
                size: OpSize::Q,
                operands: vec![operand],
                def: None,
            });
        }
        for source in &long_lived {
            let result = mf.new_vreg(X86RegClass::Gp64);
            mf.blocks[0].insts.push(X86Inst {
                opcode: X86Opcode::MovRR,
                size: OpSize::Q,
                operands: vec![X86Operand::VReg(*source)],
                def: Some(X86Operand::VReg(result)),
            });
        }

        let liveness = compute_liveness(&mf);
        assert_eq!(liveness.call_positions.len(), COUNT);
        for vreg in long_lived {
            let interval = liveness
                .intervals
                .iter()
                .find(|interval| interval.vreg == vreg.id)
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
        let mf = select_function(&func, &[], crate::target::TargetLayout::LP64);
        let result = compute_liveness(&mf);
        assert!(result.num_positions > 0);
    }
}
