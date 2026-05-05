//! Linear scan register allocator.
//!
//! Replaces the naive spill-everything strategy with proper register assignment.
//! Algorithm: sort live intervals by start, walk in order, assign physical registers.
//! When no register is available, spill the interval ending furthest (or the current
//! one if it ends first). Insert ldr/str for spill code.
//!
//! Callee-saved registers (x19-x28, d8-d15) are tracked — if used, the function
//! prologue/epilogue must save/restore them.

use super::liveness::compute_liveness;
use super::mir::*;
use std::collections::{HashMap, HashSet};

/// GP registers available for allocation (excludes x18, x29, x30, x31/sp).
/// Ordered: caller-saved first (prefer these to avoid save/restore overhead),
/// then callee-saved.
///
/// x8 is reserved for large-offset addressing during emission.
/// x16, x17 are the linker scratch registers (clobbered by dynamic
/// dispatch stubs).
///
/// x9, x10, x11 are reserved **exclusively** for spill reloads
/// (see GP_SPILL_SCRATCH). They used to be in the allocation pool
/// under the theory that spill code could "borrow" them when
/// they're temporarily free, but that assumption is only valid
/// when the reload is inserted at a point where the borrowed
/// register is dead — and the spill code had no reliable way to
/// determine that. The result was a reload clobbering a
/// freshly-computed live value (the slice-print crash at -O1+).
/// Pinning x9-x11 out of the allocation pool costs 3 vregs worth
/// of pressure and guarantees reloads never clash.
const GP_ALLOC_ORDER: [u8; 22] = [
    // Caller-saved (temporary, no save needed)
    12, 13, 14, 15, // x12-x15
    0, 1, 2, 3, 4, 5, 6, 7, // x0-x7 (args, but available between calls)
    // Callee-saved (must save/restore if used)
    19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
];

/// FP registers available for allocation. Same rationale as GP:
/// d29, d30, d31 are reserved exclusively as spill reload scratch.
const FP_ALLOC_ORDER: [u8; 29] = [
    // Caller-saved
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 0, 1, 2, 3, 4, 5, 6, 7,
    // Callee-saved
    8, 9, 10, 11, 12, 13, 14, 15,
];

/// Callee-saved GP register range.
const GP_CALLEE_SAVED: std::ops::RangeInclusive<u8> = 19..=28;
/// Callee-saved FP register range.
const FP_CALLEE_SAVED: std::ops::RangeInclusive<u8> = 8..=15;

/// Bytes a single spill of a vreg of class `class` occupies. NEON
/// vectors need 16; everything else fits in 8. Used to size frame
/// slots so the LdrQ/StrQ pair on a V128 spill operates on a slot
/// that's actually 16 bytes wide and 16-byte aligned.
fn spill_slot_size(class: RegClass) -> u32 {
    match class {
        RegClass::V128 => 16,
        _ => 8,
    }
}

/// Where a split interval's *post-call* half lives. `Allocated`
/// is the win — the post-half got a register, so the only memory
/// traffic is the str/ldr that bridges the call.  `Spilled` is the
/// fall-back: the post-half couldn't be allocated to a register
/// either, so post-call uses load directly from `bridge_slot`
/// (which is the same slot the pre-half wrote into before the
/// call), and we omit the redundant ldr.
#[derive(Debug, Clone, Copy)]
pub enum PostHalf {
    Allocated(PhysReg),
    Spilled,
}

/// One record per call boundary at which a live range was split.
/// Drives `apply_allocation` to (a) rewrite operands of `vreg`
/// based on whether they precede or follow the call, and the
/// post-pass `insert_split_bridges` to (b) emit `str pre_phys,
/// [bridge_slot]` before the call plus, if the post-half got a
/// register, `ldr post_phys, [bridge_slot]` after.
///
/// `call_position` is the BL/BLR's instruction position in the
/// liveness numbering — used by apply_allocation to compare with
/// each operand's cur_pos. `call_index` is the same call's
/// 0-based walk-order ordinal among all calls in the function —
/// used by insert_split_bridges to match the BL/BLR by ordinal,
/// since intervening passes (apply_allocation's spill code) shift
/// numerical positions but preserve walk order.
#[derive(Debug, Clone)]
pub struct SplitRecord {
    pub vreg: VRegId,
    pub call_position: u32,
    pub call_index: u32,
    pub pre_phys: PhysReg,
    pub post: PostHalf,
    pub bridge_slot: i32,
}

/// Result of register allocation.
pub struct AllocResult {
    /// VRegId → assigned PhysReg. For a split vreg, this holds the
    /// pre-call (first-half) physreg; the post-half lives in the
    /// matching `SplitRecord`.
    pub assignments: HashMap<VRegId, PhysReg>,
    /// Spilled vregs → stack frame offset.
    pub spills: HashMap<VRegId, i32>,
    /// Callee-saved registers that were used and need save/restore.
    pub callee_saved_used: Vec<PhysReg>,
    /// Live-range splits performed at call boundaries. Sorted by
    /// `(vreg, call_position)` for deterministic lookup.
    pub split_records: Vec<SplitRecord>,
}

/// Run linear scan register allocation on a machine function.
pub fn linear_scan(mf: &mut MachineFunction) -> AllocResult {
    let liveness = compute_liveness(mf);

    let mut assignments: HashMap<VRegId, PhysReg> = HashMap::new();
    let mut spills: HashMap<VRegId, i32> = HashMap::new();
    // Active intervals: (reg_num, interval_end, current_vreg). The
    // vreg is tracked here so spill victim selection can identify
    // the *current* holder of a physical register without iterating
    // the `assignments` HashMap (which accumulates stale historical
    // entries when registers are reused after expiry, and whose
    // iteration order is non-deterministic). Audit fix surfaced by
    // mem2reg work — produced flaky `select_type.f90` failures and
    // non-reproducible builds across compiles.
    let mut active_gp: Vec<(u8, u32, VRegId)> = Vec::new();
    let mut active_fp: Vec<(u8, u32, VRegId)> = Vec::new();
    // Two free pools per class. Non-call-crossing intervals draw
    // from caller-saved first (no save/restore overhead in the
    // prologue/epilogue); the callee-saved tier is reserved for
    // call-crossing intervals, with caller-tier exhaustion as a
    // fall-through. Previously this was a single Vec consumed via
    // pop(), which returned the *last* element first — quietly
    // inverting the policy documented above on GP_ALLOC_ORDER and
    // forcing every function with even a single allocated vreg to
    // mark x28/x27/... as callee_saved_used.
    let split_pool = |order: &[u8], callee: &std::ops::RangeInclusive<u8>| -> (Vec<u8>, Vec<u8>) {
        let mut caller = Vec::new();
        let mut callees = Vec::new();
        for &r in order {
            if callee.contains(&r) {
                callees.push(r);
            } else {
                caller.push(r);
            }
        }
        (caller, callees)
    };
    let (mut free_gp_caller, mut free_gp_callee) =
        split_pool(&GP_ALLOC_ORDER, &GP_CALLEE_SAVED);
    let (mut free_fp_caller, mut free_fp_callee) =
        split_pool(&FP_ALLOC_ORDER, &FP_CALLEE_SAVED);
    let mut callee_saved_used: HashSet<PhysReg> = HashSet::new();

    // Tracks live-range splits in progress: each entry maps a
    // synthetic post-half vreg to (original vreg, call position,
    // bridge slot). After allocation we walk this map to
    // materialize SplitRecords using whatever physreg/slot the
    // post-half ended up with.
    let mut splits_in_progress: HashMap<VRegId, (VRegId, u32, i32)> = HashMap::new();

    // Per-vreg actual def/use position range, computed by direct
    // walk of the MIR. Used by the splitter to sanity-check
    // crosses_call before deciding to split: the global liveness
    // dataflow has a quirk that extends every dest vreg's live
    // range backward over its own def (op0 of `mov vreg_dst,
    // vreg_src` is treated as both def and use), inflating short
    // vreg-to-vreg copy chains to span entire blocks. The
    // splitter would then see a "call crossing" for a vreg whose
    // real def/use range is well before any call. Splitting such a
    // vreg corrupts the chain. We don't fix the dataflow globally
    // — other regalloc decisions depend on the inflated ranges in
    // ways that surface latent bugs in arg-setup emission — but we
    // do consult this tighter map before committing to a split.
    let mut vreg_actual_range: HashMap<VRegId, (u32, u32)> = HashMap::new();
    {
        let mut p: u32 = 0;
        for block in &mf.blocks {
            for inst in &block.insts {
                let mut touched: Vec<VRegId> = Vec::new();
                if let Some(d) = inst.def {
                    touched.push(d);
                }
                for op in &inst.operands {
                    if let MachineOperand::VReg(v) = op {
                        touched.push(*v);
                    }
                }
                for v in touched {
                    let entry = vreg_actual_range.entry(v).or_insert((p, p));
                    if p < entry.0 {
                        entry.0 = p;
                    }
                    if p > entry.1 {
                        entry.1 = p;
                    }
                }
                p += 2;
            }
        }
    }

    // Worklist of intervals to allocate. Starts as a clone of the
    // pre-sorted liveness intervals; splitting inserts post-half
    // intervals at their sorted position so the linear-scan
    // invariant (process in start-position order) is preserved.
    let mut worklist: Vec<super::liveness::LiveInterval> = liveness.intervals.clone();
    let mut idx = 0;
    while idx < worklist.len() {
        let interval = worklist[idx].clone();
        let is_fp = matches!(interval.class, RegClass::Fp32 | RegClass::Fp64);

        if is_fp {
            expire_intervals(
                &mut active_fp,
                &mut free_fp_caller,
                &mut free_fp_callee,
                &FP_CALLEE_SAVED,
                interval.start,
            );
        } else {
            expire_intervals(
                &mut active_gp,
                &mut free_gp_caller,
                &mut free_gp_callee,
                &GP_CALLEE_SAVED,
                interval.start,
            );
        }

        let (active, free_caller, free_callee) = if is_fp {
            (&mut active_fp, &mut free_fp_caller, &mut free_fp_callee)
        } else {
            (&mut active_gp, &mut free_gp_caller, &mut free_gp_callee)
        };

        let reg_opt = if interval.crosses_call {
            // Must use callee-saved (caller-saved would be clobbered
            // by the call).
            free_callee.pop()
        } else if let Some(hint) = interval.hint {
            // Try the hinted register first across both tiers.
            if let Some(i) = free_caller.iter().position(|&r| r == hint) {
                Some(free_caller.remove(i))
            } else if let Some(i) = free_callee.iter().position(|&r| r == hint) {
                Some(free_callee.remove(i))
            } else {
                free_caller.pop().or_else(|| free_callee.pop())
            }
        } else {
            free_caller.pop().or_else(|| free_callee.pop())
        };

        // Live-range splitting: a call-crossing interval that
        // can't grab a callee-saved register would otherwise be
        // full-spilled. If it has a single call crossing AND a
        // caller-saved register is available right now, split at
        // the call boundary instead — the pre-half goes to the
        // caller-saved register, the post-half is allocated as a
        // fresh interval, and we bridge the call with str/ldr to
        // a frame slot. Net win: the longer-lived half between
        // the def and the call avoids spill loads on every use.
        // Cross-check `crosses_call` against the actual def/use
        // range from the MIR walk: a vreg whose real range doesn't
        // straddle the supposed crossing point is a false positive
        // from inflated liveness, and must NOT be split.
        let real_crosses = if let Some(&(real_start, real_end)) =
            vreg_actual_range.get(&interval.vreg)
        {
            interval.call_crossings.len() == 1
                && interval.call_crossings[0] > real_start
                && interval.call_crossings[0] < real_end
        } else {
            false
        };
        if reg_opt.is_none()
            && interval.crosses_call
            && real_crosses
            && !free_caller.is_empty()
        {
            let call_pos = interval.call_crossings[0];
            let bridge_slot = mf.alloc_local(spill_slot_size(interval.class));
            let synthetic = mf.new_vreg(interval.class);
            splits_in_progress.insert(synthetic, (interval.vreg, call_pos, bridge_slot));
            // Pre-half: shrink the original to end before the call.
            // Now non-call-crossing, so it can take a caller-saved.
            let pre_half = super::liveness::LiveInterval {
                vreg: interval.vreg,
                class: interval.class,
                start: interval.start,
                end: call_pos.saturating_sub(1),
                crosses_call: false,
                call_crossings: Vec::new(),
                hint: interval.hint,
            };
            // Post-half: a fresh synthetic vreg covering the
            // post-call range. Single original crossing → no
            // crossings remain; cannot recurse-split.
            let post_half = super::liveness::LiveInterval {
                vreg: synthetic,
                class: interval.class,
                start: call_pos.saturating_add(1),
                end: interval.end,
                crosses_call: false,
                call_crossings: Vec::new(),
                hint: None,
            };
            worklist[idx] = pre_half;
            // Insert post-half at its sorted position.
            let post_start = post_half.start;
            let post_vreg = post_half.vreg.0;
            let mut insert_at = idx + 1;
            while insert_at < worklist.len() {
                let w = &worklist[insert_at];
                if (w.start, w.vreg.0) > (post_start, post_vreg) {
                    break;
                }
                insert_at += 1;
            }
            worklist.insert(insert_at, post_half);
            // Re-process the truncated pre-half at the same index.
            continue;
        }

        if let Some(reg) = reg_opt {
            // Register available.
            let phys = if is_fp {
                match interval.class {
                    RegClass::Fp32 => PhysReg::Fp32(reg),
                    _ => PhysReg::Fp(reg),
                }
            } else {
                match interval.class {
                    RegClass::Gp32 => PhysReg::Gp32(reg),
                    _ => PhysReg::Gp(reg),
                }
            };
            assignments.insert(interval.vreg, phys);
            active.push((reg, interval.end, interval.vreg));
            active.sort_by_key(|&(_, end, _)| end);

            // Track callee-saved usage.
            if !is_fp && GP_CALLEE_SAVED.contains(&reg) {
                callee_saved_used.insert(PhysReg::Gp(reg));
            }
            if is_fp && FP_CALLEE_SAVED.contains(&reg) {
                callee_saved_used.insert(PhysReg::Fp(reg));
            }
        } else {
            // No register available — spill. Find the active
            // interval ending furthest. `active` is kept sorted by
            // end (see the `active.sort_by_key(...)` calls above
            // and below), so the last entry is always the furthest.
            // The active list carries the current vreg directly so
            // we don't have to scan the (non-deterministic,
            // stale-entry-laden) assignments map for the victim.
            if let Some(&(spill_reg, spill_end, victim)) = active.last() {
                let last_idx = active.len() - 1;
                if spill_end > interval.end {
                    // Spill the furthest active interval, give its register to us.
                    // The victim's class is what determines its slot size — not
                    // ours, since we're taking the victim's physreg.
                    let victim_class = mf
                        .vregs
                        .iter()
                        .find(|v| v.id == victim)
                        .map(|v| v.class)
                        .unwrap_or(RegClass::Gp64);
                    let offset = mf.alloc_local(spill_slot_size(victim_class));
                    spills.insert(victim, offset);
                    assignments.remove(&victim);

                    let phys = if is_fp {
                        match interval.class {
                            RegClass::Fp32 => PhysReg::Fp32(spill_reg),
                            _ => PhysReg::Fp(spill_reg),
                        }
                    } else {
                        match interval.class {
                            RegClass::Gp32 => PhysReg::Gp32(spill_reg),
                            _ => PhysReg::Gp(spill_reg),
                        }
                    };
                    assignments.insert(interval.vreg, phys);
                    active.remove(last_idx);
                    active.push((spill_reg, interval.end, interval.vreg));
                    active.sort_by_key(|&(_, end, _)| end);
                } else {
                    // Current interval ends later — spill it. Use the
                    // current interval's class for slot sizing.
                    let offset = mf.alloc_local(spill_slot_size(interval.class));
                    spills.insert(interval.vreg, offset);
                }
            } else {
                // No active intervals — shouldn't happen but spill to be safe.
                let offset = mf.alloc_local(spill_slot_size(interval.class));
                spills.insert(interval.vreg, offset);
            }
        }
        idx += 1;
    }

    // Materialize SplitRecords from the in-progress map. Each
    // synthetic post-half vreg either landed on a phys (the win
    // case) or was spilled (the fall-back). Either way we have a
    // bridge slot that links the two halves across the call.
    //
    // Build a position→call-index map from the BL/BLR walk so
    // we can attach a stable ordinal to each split. `liveness`
    // already enumerated calls in walk order, so its
    // `call_positions` would do — but liveness exposes only
    // intervals.  Re-derive locally; the call positions are
    // small.
    let call_positions_walk: Vec<u32> = {
        let mut p: u32 = 0;
        let mut cps = Vec::new();
        for block in &mf.blocks {
            for inst in &block.insts {
                if matches!(inst.opcode, ArmOpcode::Bl | ArmOpcode::Blr) {
                    cps.push(p);
                }
                p += 2;
            }
        }
        cps
    };
    let mut split_records: Vec<SplitRecord> = splits_in_progress
        .into_iter()
        .filter_map(|(synthetic, (orig, call_pos, bridge_slot))| {
            let pre_phys = *assignments.get(&orig)?;
            let post = if let Some(&p) = assignments.get(&synthetic) {
                PostHalf::Allocated(p)
            } else if spills.contains_key(&synthetic) {
                PostHalf::Spilled
            } else {
                // No allocation either way — shouldn't happen, but
                // skip the record rather than emit garbage.
                return None;
            };
            assignments.remove(&synthetic);
            spills.remove(&synthetic);
            let call_index = call_positions_walk.binary_search(&call_pos).ok()? as u32;
            Some(SplitRecord {
                vreg: orig,
                call_position: call_pos,
                call_index,
                pre_phys,
                post,
                bridge_slot,
            })
        })
        .collect();
    split_records.sort_by_key(|r| (r.vreg.0, r.call_position));

    // Sort callee-saved for consistent prologue/epilogue ordering.
    let mut callee_saved: Vec<PhysReg> = callee_saved_used.into_iter().collect();
    callee_saved.sort_by_key(|r| match r {
        PhysReg::Gp(n) | PhysReg::Gp32(n) => *n as u16,
        PhysReg::Fp(n) | PhysReg::Fp32(n) => 100 + *n as u16,
        _ => 200,
    });

    AllocResult {
        assignments,
        spills,
        split_records,
        callee_saved_used: callee_saved,
    }
}

/// Position-resolved assignment for a vreg. A vreg that wasn't
/// split has the same assignment everywhere; a split vreg's
/// assignment depends on whether the use is before or after the
/// call boundary, and the post-call assignment may itself be a
/// memory slot (the spilled-post fall-back).
enum LogicalAssignment {
    Reg(PhysReg),
    Slot(i32),
}

/// Apply allocation result: rewrite VReg operands to PhysReg, insert spill code.
/// For spilled operands, borrows a temporarily-free register from the allocation
/// pool rather than using dedicated scratch registers. This avoids wasting
/// registers in the pool and follows standard linear scan practice.
pub fn apply_allocation(
    mf: &mut MachineFunction,
    result: &AllocResult,
    liveness: &super::liveness::LivenessResult,
) {
    let vreg_classes: HashMap<VRegId, RegClass> =
        mf.vregs.iter().map(|v| (v.id, v.class)).collect();

    // Build interval lookup: for each vreg, its live range.
    let intervals: HashMap<VRegId, (u32, u32)> = liveness
        .intervals
        .iter()
        .map(|i| (i.vreg, (i.start, i.end)))
        .collect();

    // Compute instruction positions for each block/instruction.
    let mut inst_pos: HashMap<(usize, usize), u32> = HashMap::new();
    let mut pos: u32 = 0;
    for (bi, block) in mf.blocks.iter().enumerate() {
        for (ii, _) in block.insts.iter().enumerate() {
            inst_pos.insert((bi, ii), pos);
            pos += 2;
        }
    }

    // Position-aware lookup: returns the logical assignment of
    // `vid` at `cur_pos`, consulting split_records first so that
    // post-call uses pick up the post-half's location instead of
    // the pre-half's (now-clobbered) caller-saved register.
    let logical_assignment = |vid: VRegId, cur_pos: u32| -> Option<LogicalAssignment> {
        for r in &result.split_records {
            if r.vreg != vid {
                continue;
            }
            if cur_pos > r.call_position {
                return Some(match r.post {
                    PostHalf::Allocated(p) => LogicalAssignment::Reg(p),
                    PostHalf::Spilled => LogicalAssignment::Slot(r.bridge_slot),
                });
            }
            return Some(LogicalAssignment::Reg(r.pre_phys));
        }
        if let Some(p) = result.assignments.get(&vid) {
            Some(LogicalAssignment::Reg(*p))
        } else {
            result.spills.get(&vid).copied().map(LogicalAssignment::Slot)
        }
    };

    for block_idx in 0..mf.blocks.len() {
        let mut new_insts = Vec::new();
        let insts = std::mem::take(&mut mf.blocks[block_idx].insts);

        for (inst_idx, inst) in insts.iter().enumerate() {
            let mut rewritten = inst.clone();
            let cur_pos = inst_pos.get(&(block_idx, inst_idx)).copied().unwrap_or(0);

            // Find GP registers NOT in use at this position. A
            // vreg's register is in use during its full live range
            // unless the vreg was split — in which case the
            // pre-half occupies pre_phys during [start,
            // call_position] and the post-half occupies post_phys
            // during (call_position, end].
            //
            // Iterate `result.assignments` in **deterministic vreg
            // order**, not raw HashMap iteration order — the latter
            // varies between runs and produces different temp-reg
            // assignments and therefore non-reproducible builds. The
            // resulting `gp_temps`/`fp_temps` lists are consumed by
            // index, so their order is load-bearing.
            let mut gp_temps: Vec<u8> = Vec::new();
            let mut fp_temps: Vec<u8> = Vec::new();
            let mut sorted_assignments: Vec<(VRegId, PhysReg)> =
                result.assignments.iter().map(|(&v, &p)| (v, p)).collect();
            sorted_assignments.sort_by_key(|(v, _)| v.0);
            // Build the set of phys regs in use at cur_pos. For
            // split vregs, pre_phys is in use up to call_position,
            // post_phys (if Allocated) takes over after.
            let mut used_gp: HashSet<u8> = HashSet::new();
            let mut used_fp: HashSet<u8> = HashSet::new();
            let mark_used = |phys: PhysReg, gp: &mut HashSet<u8>, fp: &mut HashSet<u8>| match phys {
                PhysReg::Gp(n) | PhysReg::Gp32(n) => {
                    gp.insert(n);
                }
                PhysReg::Fp(n) | PhysReg::Fp32(n) => {
                    fp.insert(n);
                }
                _ => {}
            };
            for (vreg, phys) in &sorted_assignments {
                if let Some(&(start, end)) = intervals.get(vreg) {
                    if cur_pos < start || cur_pos > end {
                        continue;
                    }
                    // Split vregs: pre_phys covers [start,
                    // call_position]; post_phys covers
                    // (call_position, end]. assignments[vreg] is
                    // the pre_phys (we removed the synthetic).
                    let split = result.split_records.iter().find(|r| r.vreg == *vreg);
                    if let Some(rec) = split {
                        if cur_pos <= rec.call_position {
                            mark_used(rec.pre_phys, &mut used_gp, &mut used_fp);
                        }
                        if let PostHalf::Allocated(p) = rec.post {
                            if cur_pos > rec.call_position {
                                mark_used(p, &mut used_gp, &mut used_fp);
                            }
                        }
                    } else {
                        mark_used(*phys, &mut used_gp, &mut used_fp);
                    }
                }
            }
            for (_, phys) in &sorted_assignments {
                match phys {
                    PhysReg::Gp(n) | PhysReg::Gp32(n) => {
                        if !used_gp.contains(n) && !gp_temps.contains(n) {
                            gp_temps.push(*n);
                        }
                    }
                    PhysReg::Fp(n) | PhysReg::Fp32(n) => {
                        if !used_fp.contains(n) && !fp_temps.contains(n) {
                            fp_temps.push(*n);
                        }
                    }
                    _ => {}
                }
            }
            // post_phys registers from split records are also
            // candidates for temp use when *they're* not in use.
            for r in &result.split_records {
                if let PostHalf::Allocated(p) = r.post {
                    let in_use = match p {
                        PhysReg::Gp(n) | PhysReg::Gp32(n) => used_gp.contains(&n),
                        PhysReg::Fp(n) | PhysReg::Fp32(n) => used_fp.contains(&n),
                        _ => true,
                    };
                    if !in_use {
                        match p {
                            PhysReg::Gp(n) | PhysReg::Gp32(n) => {
                                if !gp_temps.contains(&n) {
                                    gp_temps.push(n);
                                }
                            }
                            PhysReg::Fp(n) | PhysReg::Fp32(n) => {
                                if !fp_temps.contains(&n) {
                                    fp_temps.push(n);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Also include the dedicated fallback scratches in case no free regs found.
            for &s in &GP_SPILL_SCRATCH {
                if !gp_temps.contains(&s) {
                    gp_temps.push(s);
                }
            }
            for &s in &FP_SPILL_SCRATCH {
                if !fp_temps.contains(&s) {
                    fp_temps.push(s);
                }
            }

            // For spilled (or post-spilled-after-split) vreg uses:
            // borrow a temp register and load from the slot.
            let mut loads = Vec::new();
            let mut gp_temp_idx = 0usize;
            let mut fp_temp_idx = 0usize;
            for (i, op) in inst.operands.iter().enumerate() {
                if let MachineOperand::VReg(vid) = op {
                    if let Some(LogicalAssignment::Slot(offset)) =
                        logical_assignment(*vid, cur_pos)
                    {
                        let class = vreg_classes.get(vid).copied().unwrap_or(RegClass::Gp64);
                        let (temp_reg, load_op) =
                            if matches!(class, RegClass::Fp32 | RegClass::Fp64) {
                                let r = fp_temps
                                    .get(fp_temp_idx)
                                    .copied()
                                    .unwrap_or(FP_SPILL_SCRATCH[0]);
                                fp_temp_idx += 1;
                                let phys = if matches!(class, RegClass::Fp32) {
                                    PhysReg::Fp32(r)
                                } else {
                                    PhysReg::Fp(r)
                                };
                                (phys, ArmOpcode::LdrFpImm)
                            } else {
                                let r = gp_temps
                                    .get(gp_temp_idx)
                                    .copied()
                                    .unwrap_or(GP_SPILL_SCRATCH[0]);
                                gp_temp_idx += 1;
                                let phys = if matches!(class, RegClass::Gp32) {
                                    PhysReg::Gp32(r)
                                } else {
                                    PhysReg::Gp(r)
                                };
                                (phys, ArmOpcode::LdrImm)
                            };
                        loads.push((i, temp_reg, load_op, offset));
                    }
                }
            }

            for (op_idx, scratch, load_op, offset) in &loads {
                if *op_idx == 0
                    && inst
                        .def
                        .as_ref()
                        .map(|d| {
                            matches!(
                                logical_assignment(*d, cur_pos),
                                Some(LogicalAssignment::Slot(_))
                            )
                        })
                        .unwrap_or(false)
                {
                    continue;
                }
                new_insts.push(MachineInst {
                    opcode: *load_op,
                    operands: vec![
                        MachineOperand::PhysReg(*scratch),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(*offset as i64),
                    ],
                    def: None,
                });
                rewritten.operands[*op_idx] = MachineOperand::PhysReg(*scratch);
            }

            // Rewrite assigned vregs to physical registers using
            // the position-aware lookup so split vregs pick up the
            // correct half.
            for op in &mut rewritten.operands {
                if let MachineOperand::VReg(vid) = op {
                    if let Some(LogicalAssignment::Reg(phys)) =
                        logical_assignment(*vid, cur_pos)
                    {
                        *op = MachineOperand::PhysReg(phys);
                    }
                }
            }

            // Handle def — use a temp that doesn't alias any input temp.
            let def_temp_idx = gp_temp_idx.max(fp_temp_idx);
            let def_spill = if let Some(def_vid) = &inst.def {
                if let Some(LogicalAssignment::Slot(offset)) =
                    logical_assignment(*def_vid, cur_pos)
                {
                    let class = vreg_classes.get(def_vid).copied().unwrap_or(RegClass::Gp64);
                    let temp_reg = if matches!(class, RegClass::Fp32 | RegClass::Fp64) {
                        let r = fp_temps
                            .get(def_temp_idx)
                            .copied()
                            .unwrap_or(FP_SPILL_SCRATCH[0]);
                        if matches!(class, RegClass::Fp32) {
                            PhysReg::Fp32(r)
                        } else {
                            PhysReg::Fp(r)
                        }
                    } else {
                        let r = gp_temps
                            .get(def_temp_idx)
                            .copied()
                            .unwrap_or(GP_SPILL_SCRATCH[0]);
                        if matches!(class, RegClass::Gp32) {
                            PhysReg::Gp32(r)
                        } else {
                            PhysReg::Gp(r)
                        }
                    };
                    let scratch = temp_reg;
                    // Replace def operand with scratch.
                    if let Some(MachineOperand::VReg(vid)) = rewritten.operands.first() {
                        if vid == def_vid {
                            rewritten.operands[0] = MachineOperand::PhysReg(scratch);
                        }
                    }
                    Some((scratch, offset, class))
                } else {
                    None
                }
            } else {
                None
            };

            // Bridge instructions for live-range splits are
            // emitted by `insert_split_bridges`, which runs *after*
            // `parallelize_call_arg_moves` so the canonical
            // arg-setup mov sequence isn't fragmented by an
            // interposed store and the parallel-copy resolver can
            // see all the arg-setup movs as one block.
            rewritten.def = None;
            new_insts.push(rewritten);

            // Store after def for spilled vregs.
            if let Some((scratch, offset, class)) = def_spill {
                let store_op = match class {
                    RegClass::Fp32 | RegClass::Fp64 => ArmOpcode::StrFpImm,
                    _ => ArmOpcode::StrImm,
                };
                new_insts.push(MachineInst {
                    opcode: store_op,
                    operands: vec![
                        MachineOperand::PhysReg(scratch),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(offset as i64),
                    ],
                    def: None,
                });
            }
        }

        mf.blocks[block_idx].insts = new_insts;
    }
}

/// Insert callee-saved register saves in prologue and restores in epilogue.
/// Must be called after apply_allocation so we know which callee-saved regs were used.
pub fn insert_callee_saves(mf: &mut MachineFunction, callee_saved: &[PhysReg]) {
    if callee_saved.is_empty() {
        return;
    }

    // Allocate stack slots for callee-saved registers.
    let mut save_slots: Vec<(PhysReg, i32)> = Vec::new();
    for &reg in callee_saved {
        let offset = mf.frame.alloc_local(8);
        save_slots.push((reg, offset));
    }

    // Insert saves at the start of the entry block (after prologue setup).
    // Find the insertion point: after the STP + ADD (prologue) instructions.
    let prologue_end = mf.blocks[0]
        .insts
        .iter()
        .position(|i| {
            // The prologue is StpPre followed by AddImm. Insert after those.
            !matches!(i.opcode, ArmOpcode::StpPre | ArmOpcode::AddImm)
        })
        .unwrap_or(0);

    // Emit saves, pairing consecutive same-class registers into STP.
    // save_slots are ordered by increasing register number; slots are
    // allocated at decreasing offsets (-8, -16, -24, ...). Adjacent
    // slots differ by 8 bytes — exactly the STP stride for GP64/FP64.
    //
    // STP Xt1, Xt2, [Xn, #off]: stores Xt1 at Xn+off, Xt2 at Xn+off+8.
    // So to store reg_low (at lower_offset) and reg_high (at lower_offset+8):
    //   STP reg_low, reg_high, [FP, #lower_offset]
    let saves = emit_callee_store_pairs(&save_slots, false);
    for (i, save) in saves.into_iter().enumerate() {
        mf.blocks[0].insts.insert(prologue_end + i, save);
    }

    // Insert restores before every epilogue sequence (LdpPost).
    for block in &mut mf.blocks {
        let mut insertions = Vec::new();
        for (i, inst) in block.insts.iter().enumerate() {
            if inst.opcode == ArmOpcode::LdpPost {
                insertions.push(i);
            }
        }
        for &ldp_idx in insertions.iter().rev() {
            // Restores in reverse order (mirror of saves).
            let restores = emit_callee_store_pairs(&save_slots, true);
            for restore in restores.into_iter().rev() {
                block.insts.insert(ldp_idx, restore);
            }
        }
    }
}

/// Build the save (or restore) instruction list for a set of callee-saved
/// slots, pairing consecutive adjacent GP/FP slots into STP/LDP.
///
/// `restore` = true → emit LDP/LDR (load); false → STP/STR (store).
///
/// Pairing rule: slots[i] and slots[i+1] are paired when:
///   * Both are GP or both are FP (same register class)
///   * slots[i+1].offset == slots[i].offset - 8 (adjacent 8-byte slots)
///
/// STP Xt1, Xt2, [Xn, #off] → Xt1 at off, Xt2 at off+8.
/// We pair as: STP slots[i+1].reg, slots[i].reg, [FP, #slots[i+1].offset]
/// because slots[i+1] has the lower (more negative) offset.
fn emit_callee_store_pairs(save_slots: &[(PhysReg, i32)], restore: bool) -> Vec<MachineInst> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < save_slots.len() {
        let (reg1, off1) = save_slots[i];
        // Try to pair with next slot.
        if i + 1 < save_slots.len() {
            let (reg2, off2) = save_slots[i + 1];
            let is_gp1 = !matches!(reg1, PhysReg::Fp(_) | PhysReg::Fp32(_));
            let is_gp2 = !matches!(reg2, PhysReg::Fp(_) | PhysReg::Fp32(_));
            let same_class = is_gp1 == is_gp2;
            // off1 is the higher slot (less negative); off2 = off1 - 8.
            let adjacent = off2 == off1 - 8;
            // STP offset must fit in 7-bit signed × 8: range -512..504.
            let in_range = (-512..=504).contains(&off2);
            if same_class && adjacent && in_range {
                let opcode = if restore {
                    ArmOpcode::LdpOffset
                } else {
                    ArmOpcode::StpOffset
                };
                let (low_reg, high_reg) = (reg2, reg1);
                result.push(MachineInst {
                    opcode,
                    operands: vec![
                        MachineOperand::PhysReg(low_reg),
                        MachineOperand::PhysReg(high_reg),
                        MachineOperand::PhysReg(PhysReg::FP),
                        MachineOperand::Imm(off2 as i64),
                    ],
                    def: None,
                });
                i += 2;
                continue;
            }
        }
        // Emit individual STR/LDR.
        let (op, is_fp) = match reg1 {
            PhysReg::Fp(_) | PhysReg::Fp32(_) => {
                if restore {
                    (ArmOpcode::LdrFpImm, true)
                } else {
                    (ArmOpcode::StrFpImm, true)
                }
            }
            _ => {
                if restore {
                    (ArmOpcode::LdrImm, false)
                } else {
                    (ArmOpcode::StrImm, false)
                }
            }
        };
        let _ = is_fp;
        result.push(MachineInst {
            opcode: op,
            operands: vec![
                MachineOperand::PhysReg(reg1),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(off1 as i64),
            ],
            def: if restore {
                Some(match reg1 {
                    PhysReg::Gp(n) | PhysReg::Gp32(n) => crate::codegen::mir::VRegId(n.into()),
                    PhysReg::Fp(n) | PhysReg::Fp32(n) => crate::codegen::mir::VRegId(n.into()),
                    _ => crate::codegen::mir::VRegId(0),
                })
            } else {
                None
            },
        });
        i += 1;
    }
    result
}

/// Basic move coalescing: eliminate mov instructions where src == dest.
pub fn coalesce_moves(mf: &mut MachineFunction) {
    for block in &mut mf.blocks {
        block.insts.retain(|inst| {
            if matches!(inst.opcode, ArmOpcode::MovReg | ArmOpcode::FmovReg)
                && inst.operands.len() == 2
                && inst.operands[0] == inst.operands[1]
            {
                return false; // eliminate self-move
            }
            true
        });
    }
}

/// Emit the str/ldr pairs that bridge live-range splits across
/// each call boundary. Runs *after* `parallelize_call_arg_moves`
/// so the parallel-copy resolver sees an unfragmented arg-setup
/// sequence (an interposed `str` between an arg-copy and the BL
/// would otherwise stop the scan-back at the wrong point and the
/// resolver would miss arg-copies that need cycle-breaking).
///
/// For each `SplitRecord` whose `call_position` equals the
/// position of a `Bl`/`Blr` in the function: insert a `str
/// pre_phys, [fp, bridge_slot]` immediately before the call and,
/// if the post-half got its own register, an `ldr post_phys, [fp,
/// bridge_slot]` immediately after.  Spilled post-halves rely on
/// the existing per-use spill-load path, which already loads from
/// `bridge_slot` thanks to `apply_allocation`'s position-aware
/// operand rewrite.
pub fn insert_split_bridges(mf: &mut MachineFunction, splits: &[SplitRecord]) {
    if splits.is_empty() {
        return;
    }
    let mut call_seen: u32 = 0;
    for block_idx in 0..mf.blocks.len() {
        let mut new_insts = Vec::with_capacity(mf.blocks[block_idx].insts.len());
        let original = std::mem::take(&mut mf.blocks[block_idx].insts);
        for inst in original.into_iter() {
            let is_call = matches!(inst.opcode, ArmOpcode::Bl | ArmOpcode::Blr);
            if is_call {
                let idx = call_seen;
                for r in splits {
                    if r.call_index != idx {
                        continue;
                    }
                    let store_op = match r.pre_phys {
                        PhysReg::Fp(_) | PhysReg::Fp32(_) => ArmOpcode::StrFpImm,
                        _ => ArmOpcode::StrImm,
                    };
                    new_insts.push(MachineInst {
                        opcode: store_op,
                        operands: vec![
                            MachineOperand::PhysReg(r.pre_phys),
                            MachineOperand::PhysReg(PhysReg::FP),
                            MachineOperand::Imm(r.bridge_slot as i64),
                        ],
                        def: None,
                    });
                }
            }
            new_insts.push(inst);
            if is_call {
                let idx = call_seen;
                for r in splits {
                    if r.call_index != idx {
                        continue;
                    }
                    if let PostHalf::Allocated(p) = r.post {
                        let load_op = match p {
                            PhysReg::Fp(_) | PhysReg::Fp32(_) => ArmOpcode::LdrFpImm,
                            _ => ArmOpcode::LdrImm,
                        };
                        new_insts.push(MachineInst {
                            opcode: load_op,
                            operands: vec![
                                MachineOperand::PhysReg(p),
                                MachineOperand::PhysReg(PhysReg::FP),
                                MachineOperand::Imm(r.bridge_slot as i64),
                            ],
                            def: None,
                        });
                    }
                }
                call_seen += 1;
            }
        }
        mf.blocks[block_idx].insts = new_insts;
    }
}

/// Resolve the entry-block argument-receipt parallel copy. The
/// function prologue copies incoming arg registers (x0..x7,
/// d0..d7) into the registers that the allocator chose for the
/// parameter vregs. Emitted naively this is a sequential `mov`
/// chain, which corrupts values whenever a destination aliases a
/// later source — e.g. `mov x4, x3; mov x3, x4` clobbers x4 before
/// the second move can read it. Historically this was hidden
/// because parameter vregs landed in callee-saved registers
/// (x19..x28) that never alias x0..x7; once the allocator started
/// preferring caller-saved destinations, the latent hazard
/// surfaced.
///
/// Find the leading run of phys-to-phys movs in the entry block
/// (after the frame setup) whose source is an incoming arg
/// register, and run the same parallel-copy resolver
/// (`rewrite_call_arg_copies`) used for pre-call argument setup.
pub fn parallelize_entry_arg_moves(mf: &mut MachineFunction) {
    if mf.blocks.is_empty() {
        return;
    }
    let original = std::mem::take(&mut mf.blocks[0].insts);
    let mut rebuilt: Vec<MachineInst> = Vec::with_capacity(original.len());
    let mut iter = original.into_iter().peekable();

    // Pass through the prologue (StpPre/AddImm/SubImm).
    while let Some(inst) = iter.peek() {
        if matches!(
            inst.opcode,
            ArmOpcode::StpPre | ArmOpcode::AddImm | ArmOpcode::SubImm
        ) {
            rebuilt.push(iter.next().unwrap());
        } else {
            break;
        }
    }

    // Process arg-receipt parallel groups. Tolerate intervening
    // store instructions (which read but don't write registers in
    // the parallel-copy graph) so a result-address spill or
    // partial-receipt store doesn't fragment the receipts. Stop
    // once any instruction overwrites an incoming arg register —
    // beyond that point those values are no longer the original
    // arg values and parallel-copy resolution would be unsound.
    let mut pending: Vec<MachineInst> = Vec::new();
    loop {
        match iter.peek() {
            None => break,
            Some(inst) if is_arg_receipt_copy(inst) => {
                pending.push(iter.next().unwrap());
            }
            Some(inst) if is_transparent_to_arg_receipts(inst) => {
                if !pending.is_empty() {
                    rebuilt.extend(rewrite_call_arg_copies(std::mem::take(&mut pending)));
                }
                rebuilt.push(iter.next().unwrap());
            }
            Some(_) => break,
        }
    }
    if !pending.is_empty() {
        rebuilt.extend(rewrite_call_arg_copies(pending));
    }
    rebuilt.extend(iter);
    mf.blocks[0].insts = rebuilt;
}

/// An instruction is *transparent* to the arg-receipt parallel
/// copy if executing it cannot change the value held in any
/// incoming arg register (x0..x7 / d0..d7). Stores qualify (they
/// only read), as does any compute whose destination lies outside
/// the arg-reg range. Anything that writes into x0..x7 forms a
/// barrier — the receipts that come after it are reading
/// post-clobber values, so reordering across the barrier is
/// unsound.
fn is_transparent_to_arg_receipts(inst: &MachineInst) -> bool {
    if is_store_opcode(inst.opcode) {
        return true;
    }
    if is_branch_or_call_opcode(inst.opcode) {
        return false;
    }
    match inst.operands.first() {
        Some(MachineOperand::PhysReg(dst)) => !is_call_arg_reg(*dst),
        _ => true,
    }
}

fn is_store_opcode(op: ArmOpcode) -> bool {
    matches!(
        op,
        ArmOpcode::StrImm
            | ArmOpcode::StrhImm
            | ArmOpcode::StrbImm
            | ArmOpcode::StrFpImm
            | ArmOpcode::StrReg
            | ArmOpcode::StrFpReg
            | ArmOpcode::StrQ
            | ArmOpcode::StpPre
            | ArmOpcode::StpOffset
    )
}

fn is_branch_or_call_opcode(op: ArmOpcode) -> bool {
    matches!(
        op,
        ArmOpcode::B
            | ArmOpcode::BCond
            | ArmOpcode::Bl
            | ArmOpcode::Blr
            | ArmOpcode::Ret
            | ArmOpcode::Cbz
            | ArmOpcode::Cbnz
            | ArmOpcode::Tbz
            | ArmOpcode::Tbnz
    )
}

/// Reorder physical argument-register copies immediately before calls so later
/// sources are not clobbered by earlier destination writes.
///
/// For an indirect call (`BLR Xn`), the target register `Xn` is a
/// live use of the call. If `Xn` lies in the argument-register
/// range (x0..x7) and one of the pending arg-copy moves writes to
/// it, the function pointer would be clobbered before the branch.
/// Save it to a non-arg scratch register (`x10`, distinct from
/// `x9` that `rewrite_call_arg_copies` uses for cycle-breaking) and
/// rewrite the BLR's operand. This was hidden as long as
/// indirect-call targets landed in callee-saved registers (which
/// can never alias x0..x7) but surfaces once non-call-crossing
/// values prefer caller-saved.
pub fn parallelize_call_arg_moves(mf: &mut MachineFunction) {
    for block in &mut mf.blocks {
        let mut rebuilt: Vec<MachineInst> = Vec::with_capacity(block.insts.len());
        for inst in std::mem::take(&mut block.insts) {
            if matches!(inst.opcode, ArmOpcode::Bl | ArmOpcode::Blr) {
                let mut start = rebuilt.len();
                while start > 0 && is_call_arg_copy(&rebuilt[start - 1]) {
                    start -= 1;
                }
                let mut adjusted = inst;
                if matches!(adjusted.opcode, ArmOpcode::Blr) {
                    let target_phys = match adjusted.operands.first() {
                        Some(MachineOperand::PhysReg(t)) => Some(*t),
                        _ => None,
                    };
                    if let Some(target) = target_phys {
                        if is_call_arg_reg(target) {
                            let target_alias = phys_reg_alias(target);
                            let writes_target = rebuilt[start..].iter().any(|i| {
                                matches!(i.operands.first(),
                                    Some(MachineOperand::PhysReg(d))
                                        if phys_reg_alias(*d) == target_alias)
                            });
                            if writes_target {
                                let scratch = blr_target_scratch(target);
                                let save = MachineInst {
                                    opcode: move_opcode_for_phys(target),
                                    operands: vec![
                                        MachineOperand::PhysReg(scratch),
                                        MachineOperand::PhysReg(target),
                                    ],
                                    def: None,
                                };
                                rebuilt.insert(start, save);
                                adjusted.operands[0] = MachineOperand::PhysReg(scratch);
                                start += 1;
                            }
                        }
                    }
                }
                if start < rebuilt.len() {
                    let pending = rebuilt.split_off(start);
                    rebuilt.extend(rewrite_call_arg_copies(pending));
                }
                rebuilt.push(adjusted);
            } else {
                rebuilt.push(inst);
            }
        }
        block.insts = rebuilt;
    }
}

/// Scratch used to preserve a BLR target across parallel arg
/// setup. Distinct from the `x9` / `d29` used by
/// `rewrite_call_arg_copies` for cycle-breaking so the two scratch
/// uses never collide.
fn blr_target_scratch(target: PhysReg) -> PhysReg {
    match target {
        PhysReg::Gp(_) => PhysReg::Gp(10),
        PhysReg::Gp32(_) => PhysReg::Gp32(10),
        _ => panic!("blr_target_scratch: BLR target is always a 64/32-bit GP register"),
    }
}

// ---- Helpers ----

fn is_call_arg_copy(inst: &MachineInst) -> bool {
    matches!(inst.opcode, ArmOpcode::MovReg | ArmOpcode::FmovReg)
        && matches!(
            inst.operands.as_slice(),
            [MachineOperand::PhysReg(dst), MachineOperand::PhysReg(src)]
                if is_call_arg_reg(*dst) && !matches!(src, PhysReg::Xzr | PhysReg::Wzr)
        )
}

/// Mirror of `is_call_arg_copy` for the function-entry direction:
/// a phys-to-phys mov whose *source* is an incoming arg register.
fn is_arg_receipt_copy(inst: &MachineInst) -> bool {
    matches!(inst.opcode, ArmOpcode::MovReg | ArmOpcode::FmovReg)
        && matches!(
            inst.operands.as_slice(),
            [MachineOperand::PhysReg(_), MachineOperand::PhysReg(src)]
                if is_call_arg_reg(*src)
        )
}

fn is_call_arg_reg(reg: PhysReg) -> bool {
    match reg {
        PhysReg::Gp(n) | PhysReg::Gp32(n) => n < 8,
        PhysReg::Fp(n) | PhysReg::Fp32(n) => n < 8,
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PhysRegAlias {
    Gp(u8),
    Fp(u8),
}

fn phys_reg_alias(reg: PhysReg) -> Option<PhysRegAlias> {
    match reg {
        PhysReg::Gp(n) | PhysReg::Gp32(n) => Some(PhysRegAlias::Gp(n)),
        PhysReg::Fp(n) | PhysReg::Fp32(n) => Some(PhysRegAlias::Fp(n)),
        _ => None,
    }
}

fn scratch_phys_for(reg: PhysReg) -> PhysReg {
    match reg {
        PhysReg::Gp(_) => PhysReg::Gp(9),
        PhysReg::Gp32(_) => PhysReg::Gp32(9),
        PhysReg::Fp(_) => PhysReg::Fp(29),
        PhysReg::Fp32(_) => PhysReg::Fp32(29),
        _ => panic!("call-arg scratch requested for non-register operand"),
    }
}

fn move_opcode_for_phys(reg: PhysReg) -> ArmOpcode {
    match reg {
        PhysReg::Fp(_) | PhysReg::Fp32(_) => ArmOpcode::FmovReg,
        PhysReg::Gp(_) | PhysReg::Gp32(_) => ArmOpcode::MovReg,
        _ => panic!("move opcode requested for non-register operand"),
    }
}

fn rewrite_call_arg_copies(pending_moves: Vec<MachineInst>) -> Vec<MachineInst> {
    let mut pending: Vec<(ArmOpcode, PhysReg, PhysReg)> = pending_moves
        .into_iter()
        .map(|inst| match inst.operands.as_slice() {
            [MachineOperand::PhysReg(dst), MachineOperand::PhysReg(src)] => {
                (inst.opcode, *dst, *src)
            }
            _ => panic!("call-arg copy rewrite saw unexpected operand shape"),
        })
        .collect();
    let mut rewritten = Vec::with_capacity(pending.len() + 1);

    while !pending.is_empty() {
        let safe_idx = (0..pending.len()).find(|&i| {
            let (_, dst, _) = pending[i];
            let dst_alias = phys_reg_alias(dst).expect("call-arg copy dst should alias");
            !pending
                .iter()
                .enumerate()
                .any(|(j, &(_, _, src))| j != i && phys_reg_alias(src) == Some(dst_alias))
        });

        if let Some(idx) = safe_idx {
            let (opcode, dst, src) = pending.remove(idx);
            rewritten.push(MachineInst {
                opcode,
                operands: vec![MachineOperand::PhysReg(dst), MachineOperand::PhysReg(src)],
                def: None,
            });
            continue;
        }

        let (_, _, src) = pending[0];
        let scratch = scratch_phys_for(src);
        rewritten.push(MachineInst {
            opcode: move_opcode_for_phys(src),
            operands: vec![MachineOperand::PhysReg(scratch), MachineOperand::PhysReg(src)],
            def: None,
        });
        pending[0].2 = scratch;
    }

    rewritten
}

fn expire_intervals(
    active: &mut Vec<(u8, u32, VRegId)>,
    free_caller: &mut Vec<u8>,
    free_callee: &mut Vec<u8>,
    callee_range: &std::ops::RangeInclusive<u8>,
    pos: u32,
) {
    let mut i = 0;
    while i < active.len() {
        if active[i].1 < pos {
            let (reg, _, _) = active.remove(i);
            if callee_range.contains(&reg) {
                free_callee.push(reg);
            } else {
                free_caller.push(reg);
            }
        } else {
            i += 1;
        }
    }
}

/// Scratch registers for spill code. Multiple scratches needed when an
/// instruction has multiple spilled operands.
const GP_SPILL_SCRATCH: [u8; 3] = [9, 10, 11];
const FP_SPILL_SCRATCH: [u8; 3] = [29, 30, 31];

fn spill_scratch(class: RegClass, idx: usize) -> (PhysReg, ArmOpcode) {
    match class {
        RegClass::Fp64 => (PhysReg::Fp(FP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrFpImm),
        RegClass::Fp32 => (
            PhysReg::Fp32(FP_SPILL_SCRATCH[idx % 3]),
            ArmOpcode::LdrFpImm,
        ),
        // 128-bit vector spills/fills via LdrQ — same FP scratch
        // bank (the V registers ARE the 128-bit form of the same
        // physical D/S registers).
        RegClass::V128 => (PhysReg::Fp(FP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrQ),
        RegClass::Gp32 => (PhysReg::Gp32(GP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrImm),
        RegClass::Gp64 => (PhysReg::Gp(GP_SPILL_SCRATCH[idx % 3]), ArmOpcode::LdrImm),
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
    fn linear_scan_assigns_registers() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        let mut mf = select_function(&func);
        let result = linear_scan(&mut mf);
        // Should have assignments for the vregs.
        assert!(
            !result.assignments.is_empty(),
            "should have register assignments"
        );
        // No spills needed for 3 vregs.
        assert!(
            result.spills.is_empty(),
            "should not spill with only 3 vregs"
        );
    }

    #[test]
    fn linear_scan_no_x18() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            // Create many vregs to exercise the allocator.
            let mut vals = Vec::new();
            for i in 0..20 {
                vals.push(b.const_i32(i));
            }
            // Use them all.
            let mut sum = vals[0];
            for &v in &vals[1..] {
                sum = b.iadd(sum, v);
            }
            b.ret_void();
        }
        let mut mf = select_function(&func);
        let result = linear_scan(&mut mf);
        // x18 must never be assigned.
        for phys in result.assignments.values() {
            match phys {
                PhysReg::Gp(18) | PhysReg::Gp32(18) => {
                    panic!("x18 assigned — platform reserved!");
                }
                _ => {}
            }
        }
    }

    #[test]
    fn coalesce_eliminates_self_moves() {
        let mut mf = MachineFunction::new("test".into());
        let reg = PhysReg::Gp(9);
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![MachineOperand::PhysReg(reg), MachineOperand::PhysReg(reg)],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Ret,
            operands: vec![],
            def: None,
        });
        assert_eq!(mf.blocks[0].insts.len(), 2);
        coalesce_moves(&mut mf);
        assert_eq!(
            mf.blocks[0].insts.len(),
            1,
            "self-move should be eliminated"
        );
    }

    #[test]
    fn parallelize_call_arg_moves_preserves_later_source_registers() {
        let mut mf = MachineFunction::new("test".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(28)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::PhysReg(PhysReg::Gp(12))],
            def: None,
        });

        parallelize_call_arg_moves(&mut mf);

        assert_eq!(mf.blocks[0].insts.len(), 3);
        assert_eq!(
            mf.blocks[0].insts[0].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[1].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(28)),
            ]
        );
    }

    #[test]
    fn parallelize_call_arg_moves_breaks_cycles_with_scratch() {
        let mut mf = MachineFunction::new("test".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(1)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![MachineOperand::Extern("_callee".into())],
            def: None,
        });

        parallelize_call_arg_moves(&mut mf);

        assert_eq!(mf.blocks[0].insts.len(), 4);
        assert_eq!(
            mf.blocks[0].insts[0].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(9)),
                MachineOperand::PhysReg(PhysReg::Gp(1)),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[1].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(1)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[2].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(9)),
            ]
        );
    }

    #[test]
    fn parallelize_call_arg_moves_ignores_zero_materialization_into_arg_regs() {
        let mut mf = MachineFunction::new("test".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(4)),
                MachineOperand::PhysReg(PhysReg::Xzr),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(4)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::PhysReg(PhysReg::Gp(19))],
            def: None,
        });

        parallelize_call_arg_moves(&mut mf);

        assert_eq!(mf.blocks[0].insts.len(), 3);
        assert_eq!(
            mf.blocks[0].insts[0].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(4)),
                MachineOperand::PhysReg(PhysReg::Xzr),
            ]
        );
        assert_eq!(
            mf.blocks[0].insts[1].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(4)),
            ]
        );
    }

    #[test]
    fn linear_scan_prefers_caller_saved_when_no_calls_cross() {
        // No call-crossing intervals — every assigned vreg should
        // land on a caller-saved register and callee_saved_used
        // should stay empty (no prologue STP/LDP overhead).
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(1);
            let y = b.const_i32(2);
            let z = b.iadd(x, y);
            let _w = b.iadd(z, x);
            b.ret_void();
        }
        let mut mf = select_function(&func);
        let result = linear_scan(&mut mf);
        assert!(
            result.callee_saved_used.is_empty(),
            "no callee-saved expected for call-free function, got {:?}",
            result.callee_saved_used
        );
        for phys in result.assignments.values() {
            if let PhysReg::Gp(n) | PhysReg::Gp32(n) = phys {
                assert!(
                    !GP_CALLEE_SAVED.contains(n),
                    "vreg landed on callee-saved x{} despite caller-saved availability",
                    n
                );
            }
        }
    }

    #[test]
    fn parallelize_entry_arg_moves_resolves_swap_in_receipts() {
        let mut mf = MachineFunction::new("test".into());
        // Frame setup.
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::StpPre,
            operands: vec![],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::AddImm,
            operands: vec![],
            def: None,
        });
        // Arg-receipt swap: mov x4, x3; mov x3, x4. Naive
        // sequential emit would corrupt; the parallelizer must
        // route through scratch x9.
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(4)),
                MachineOperand::PhysReg(PhysReg::Gp(3)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(3)),
                MachineOperand::PhysReg(PhysReg::Gp(4)),
            ],
            def: None,
        });
        parallelize_entry_arg_moves(&mut mf);
        let body: Vec<&[MachineOperand]> = mf.blocks[0].insts[2..]
            .iter()
            .map(|i| i.operands.as_slice())
            .collect();
        // Expect the parallelizer to insert a scratch save then
        // emit the two receipts without clobbering.
        assert_eq!(body.len(), 3, "swap pair should expand to 3 insts");
        assert_eq!(
            body[0],
            &[
                MachineOperand::PhysReg(PhysReg::Gp(9)),
                MachineOperand::PhysReg(PhysReg::Gp(3)),
            ]
        );
    }

    #[test]
    fn parallelize_entry_arg_moves_tolerates_intervening_store() {
        let mut mf = MachineFunction::new("test".into());
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::StpPre,
            operands: vec![],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::AddImm,
            operands: vec![],
            def: None,
        });
        // mov x10, x0  (transparent: dst x10 outside arg-reg range)
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(10)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ],
            def: None,
        });
        // str x10, [fp, #-56]  (store, transparent — reads x10)
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::StrImm,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(10)),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(-56),
            ],
            def: None,
        });
        // Swap pair after the store — must still be parallelized.
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(4)),
                MachineOperand::PhysReg(PhysReg::Gp(3)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(3)),
                MachineOperand::PhysReg(PhysReg::Gp(4)),
            ],
            def: None,
        });
        parallelize_entry_arg_moves(&mut mf);
        // The store should still appear (kept in place); the swap
        // pair should have expanded to 3 movs via scratch.
        let opcodes: Vec<ArmOpcode> = mf.blocks[0].insts.iter().map(|i| i.opcode).collect();
        assert!(
            opcodes.contains(&ArmOpcode::StrImm),
            "store should be preserved across parallelization"
        );
        let mov_count = opcodes
            .iter()
            .filter(|&&op| op == ArmOpcode::MovReg)
            .count();
        assert_eq!(
            mov_count, 4,
            "expected 4 movs (1 receipt + 3 from swap expansion), got {}",
            mov_count
        );
    }

    #[test]
    fn parallelize_call_arg_moves_routes_blr_target_through_scratch() {
        let mut mf = MachineFunction::new("test".into());
        // ldr x0, [...] — function pointer loaded into x0
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::LdrImm,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::FP),
                MachineOperand::Imm(-8),
            ],
            def: None,
        });
        // mov x0, x12 — arg-setup also targets x0 (clobbers fn ptr).
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::MovReg,
            operands: vec![
                MachineOperand::PhysReg(PhysReg::Gp(0)),
                MachineOperand::PhysReg(PhysReg::Gp(12)),
            ],
            def: None,
        });
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Blr,
            operands: vec![MachineOperand::PhysReg(PhysReg::Gp(0))],
            def: None,
        });
        parallelize_call_arg_moves(&mut mf);
        // First inst should now be `mov x10, x0` (preserve the fn
        // pointer to scratch); the BLR's operand should be x10.
        assert_eq!(mf.blocks[0].insts[0].opcode, ArmOpcode::LdrImm);
        assert_eq!(mf.blocks[0].insts[1].opcode, ArmOpcode::MovReg);
        assert_eq!(
            mf.blocks[0].insts[1].operands,
            vec![
                MachineOperand::PhysReg(PhysReg::Gp(10)),
                MachineOperand::PhysReg(PhysReg::Gp(0)),
            ]
        );
        let blr = mf.blocks[0].insts.last().unwrap();
        assert_eq!(blr.opcode, ArmOpcode::Blr);
        assert_eq!(
            blr.operands,
            vec![MachineOperand::PhysReg(PhysReg::Gp(10))]
        );
    }

    #[test]
    fn linear_scan_splits_call_crossing_when_callee_saved_exhausted() {
        // 12 vregs all live across a single BL forces the
        // allocator past the 10-deep callee-saved pool. Without
        // splitting the overflow vregs full-spill; with splitting
        // they go pre-call→caller-saved → str/ldr-bridge →
        // post-call→caller-saved (so each split is two halves on
        // caller-saved registers, no whole-range memory traffic).
        let mut mf = MachineFunction::new("test".into());
        let vs: Vec<VRegId> = (0..12).map(|_| mf.new_vreg(RegClass::Gp64)).collect();
        for (i, &v) in vs.iter().enumerate() {
            mf.blocks[0].insts.push(MachineInst {
                opcode: ArmOpcode::Movz,
                operands: vec![MachineOperand::VReg(v), MachineOperand::Imm(i as i64 + 1)],
                def: Some(v),
            });
        }
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Bl,
            operands: vec![MachineOperand::Extern("_foo".into())],
            def: None,
        });
        for &v in &vs {
            mf.blocks[0].insts.push(MachineInst {
                opcode: ArmOpcode::MovReg,
                operands: vec![
                    MachineOperand::PhysReg(PhysReg::Gp(0)),
                    MachineOperand::VReg(v),
                ],
                def: None,
            });
        }
        mf.blocks[0].insts.push(MachineInst {
            opcode: ArmOpcode::Ret,
            operands: vec![],
            def: None,
        });
        let result = linear_scan(&mut mf);
        assert!(
            !result.split_records.is_empty(),
            "expected splits when 12 call-crossing vregs collide with the 10-deep callee-saved pool"
        );
        for r in &result.split_records {
            // pre-half must be on a caller-saved register; that's
            // the entire point of splitting (the callee-saved tier
            // is exhausted at split time).
            if let PhysReg::Gp(n) | PhysReg::Gp32(n) = r.pre_phys {
                assert!(
                    !GP_CALLEE_SAVED.contains(&n),
                    "split pre_phys should be caller-saved, got x{}",
                    n
                );
            }
            // bridge_slot must be a real frame offset.
            assert!(r.bridge_slot < 0, "bridge_slot should be FP-negative");
        }
    }
}
