//! Loop unrolling pass (O2+).
//!
//! Fully unrolls simple counted loops whose trip count is statically
//! known and small. Targets the common Fortran `do i = lo, hi` pattern
//! after mem2reg has converted the loop induction variable to an SSA
//! block parameter.
//!
//! ## What constitutes a "simple counted loop"
//!
//! After mem2reg runs, a canonicalized `do i = lo, hi` with stride 1
//! produces this 2-block structure:
//!
//! ```text
//! preheader:
//!     ...
//!     br header(const(lo))
//!
//! header(%i):
//!     %cmp = icmp.sle %i, const(hi)          ; or icmp.slt, hi+1
//!     condBr %cmp, latch([]), exit([])
//!
//! latch:
//!     ... body instructions using %i ...
//!     %i_next = iadd %i, const(1)
//!     br header(%i_next)
//!
//! exit:
//!     ...
//! ```
//!
//! We require:
//! - Exactly 1 latch (single back-edge).
//! - Body is exactly `{header, latch}` — no additional blocks (i.e., no IF
//!   inside the loop, no nested loops).
//! - Header has exactly 1 block parameter (the IV).
//! - IV's initial value, the loop bound, and the stride are all compile-time
//!   constants discoverable from instruction operands.
//! - Stride is exactly 1.
//! - Trip count ≤ `FULL_UNROLL_MAX` for ordinary `DO`, or
//!   `DO_CONCURRENT_FULL_UNROLL_MAX` for lowered `DO CONCURRENT`.
//! - Body instruction count ≤ `BODY_SIZE_MAX`.
//! - No instruction defined in the latch is used outside the loop (i.e., no
//!   values escape the unrolled body — they'll be dead after unrolling). This
//!   covers array loops (`a(i) = expr`) but excludes reduction loops
//!   (`s = s + a(i)`) where the accumulator IS a second block param and
//!   would escape.
//!
//! ## Transformation
//!
//! For trip count TC, we:
//! 1. Clone the latch body TC times into the preheader (substituting the IV
//!    with the constant value for that iteration).
//! 2. Rewrite the preheader's terminator to jump directly to `exit`.
//! 3. Call `prune_unreachable` to remove the now-dead header and latch.
//!
//! After this, the loop is entirely gone.

use super::pass::Pass;
use super::util::{find_natural_loops, predecessors, NaturalLoop};
use crate::ir::inst::*;
use crate::ir::types::{IntWidth, IrType};
use crate::ir::walk::prune_unreachable;
use crate::lexer::{Position, Span};
use std::collections::{HashMap, HashSet};

fn dummy_span() -> Span {
    let pos = Position { line: 0, col: 0 };
    Span {
        file_id: 0,
        start: pos,
        end: pos,
    }
}

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// Maximum trip count eligible for full unrolling.
///
/// Trip counts above this are left for sprint 29.6's partial unroller
/// which can handle dynamic trip counts with remainder loops.
const FULL_UNROLL_MAX: i64 = 8;

/// `DO CONCURRENT` is a stronger signal that iterations are independent,
/// so we allow a slightly larger full-unroll budget.
const DO_CONCURRENT_FULL_UNROLL_MAX: i64 = 16;

/// Maximum instruction count in the latch block.
///
/// Prevents code bloat from unrolling large bodies.
const BODY_SIZE_MAX: usize = 30;

// ---------------------------------------------------------------------------
// Public pass entry point
// ---------------------------------------------------------------------------

pub struct LoopUnroll;

impl Pass for LoopUnroll {
    fn name(&self) -> &'static str {
        "loop-unroll"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            changed |= unroll_in_function(func);
        }
        changed
    }
}

// ---------------------------------------------------------------------------
// Per-function entry
// ---------------------------------------------------------------------------

fn unroll_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);
    let mut changed = false;

    for nl in &loops {
        if let Some(shape) = detect_simple_loop(func, nl, &preds) {
            do_unroll(func, shape);
            prune_unreachable(func);
            changed = true;
            // After structural changes, re-running loop detection in the next
            // pass manager iteration handles any inner loops that became
            // exposed. Don't iterate here — return and let the pass manager
            // fix-point loop drive re-runs.
            break;
        }
        // Try partial unrolling for trip > FULL_UNROLL_MAX. Keeps the
        // loop intact but clones body U times and bumps step by U.
        if let Some(shape) = detect_partial_unroll_loop(func, nl, &preds) {
            do_partial_unroll(func, shape);
            changed = true;
            break;
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// Shape of a fully unrollable loop
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct LoopShape {
    preheader: BlockId,
    header: BlockId,    // block with IV block param; branches to cmp_block
    cmp_block: BlockId, // block with icmp + condBr to body/exit
    /// The computation blocks (everything that is not header, cmp_block, or
    /// latch). Ordered in CFG traversal order from cmp_block's true-successor
    /// to latch's predecessor. For most Fortran DO loops this is a single
    /// block ("do_body"). Multi-block bodies are supported as long as the
    /// sub-CFG is a linear chain with no branches to outside.
    body_blocks: Vec<BlockId>,
    latch: BlockId, // iadd IV, 1 + br header(IV_next)
    exit: BlockId,  // cond_br false target
    iv_param: ValueId,
    iv_ty: IrType,
    iv_init: i64,
    iv_bound: i64, // inclusive upper bound
    trip_count: usize,
    exit_args: Vec<ValueId>, // args from cmp_block's false branch to exit
    /// Optional second header block-param representing a reduction
    /// accumulator (sum, product, ...). When present, the unroller
    /// threads its value across iterations: each iteration's body
    /// reads the previous iteration's latch result instead of the
    /// header's join.
    reduction: Option<ReductionInfo>,
}

/// Per-loop reduction lane. mem2reg promotes a sum/product accumulator
/// into a header block param; the latch's `br header(iv_next, new_acc)`
/// passes the per-iteration update on the second lane. To unroll such a
/// loop we need to know:
///
///  * which header param carries the accumulator (`acc_param`),
///  * its preheader-supplied initial value (`acc_init`),
///  * what value the latch passes back as the new accumulator
///    (`latch_acc_value`) — that's the SSA value defined inside the
///    body that the unroller substitutes through across iterations.
#[derive(Debug, Clone, Copy)]
struct ReductionInfo {
    acc_param: ValueId,
    acc_init: ValueId,
    latch_acc_value: ValueId,
}

fn is_do_concurrent_loop(
    func: &Function,
    header: BlockId,
    cmp_block: BlockId,
    body_blocks: &[BlockId],
    latch: BlockId,
) -> bool {
    std::iter::once(header)
        .chain(std::iter::once(cmp_block))
        .chain(body_blocks.iter().copied())
        .chain(std::iter::once(latch))
        .any(|bb| func.block(bb).name.starts_with("doconc_"))
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

fn detect_simple_loop(
    func: &Function,
    nl: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<LoopShape> {
    // ---- structural requirements ----------------------------------------
    if nl.latches.len() != 1 {
        return None;
    }
    let latch = nl.latches[0];
    if latch == nl.header {
        return None;
    } // no self-loop
      // Body must have between 2 (header+latch) and 5 (header+cmp+body×N+latch) blocks.
    if nl.body.len() < 2 || nl.body.len() > 5 {
        return None;
    }

    let header = nl.header;

    // ---- header must have 1 (pure IV) or 2 (IV + reduction acc)
    //      block params ---------------------------------------------------
    let hdr = func.block(header);
    if hdr.params.is_empty() || hdr.params.len() > 2 {
        return None;
    }
    // mem2reg places the IV at index 0 and the reduction accumulator (if
    // any) at index 1. We rely on that ordering — it matches the
    // canonical lowering for `do i = 1, n; s = s + a(i); end do`.
    let iv_param_idx = 0usize;
    let acc_param_idx: Option<usize> = if hdr.params.len() == 2 { Some(1) } else { None };
    let iv_param = hdr.params[iv_param_idx].id;
    let iv_ty = hdr.params[iv_param_idx].ty.clone();
    if !matches!(iv_ty, IrType::Int(_)) {
        return None;
    }
    let acc_param: Option<ValueId> = acc_param_idx.map(|i| hdr.params[i].id);

    // ---- find preheader -------------------------------------------------
    let header_preds = preds.get(&header)?;
    let mut outside: Vec<BlockId> = header_preds
        .iter()
        .copied()
        .filter(|p| !nl.body.contains(p))
        .collect();
    outside.sort_by_key(|b| b.0);
    outside.dedup();
    if outside.len() != 1 {
        return None;
    }
    let preheader = outside[0];

    // Preheader must branch to header with exactly N args matching the
    // header's param count. The IV arg is a const; the accumulator init
    // can be any SSA value (typically a ConstInt(0) for sum, but we
    // don't enforce that — we only need to capture the value to feed
    // into iteration 0's body substitution).
    let ph_blk = func.block(preheader);
    let expected_ph_args = hdr.params.len();
    let (iv_init, acc_init) = match &ph_blk.terminator {
        Some(Terminator::Branch(dest, args))
            if *dest == header && args.len() == expected_ph_args =>
        {
            let iv_init = resolve_const_int(func, args[iv_param_idx])?;
            let acc_init = acc_param_idx.map(|i| args[i]);
            (iv_init, acc_init)
        }
        _ => return None,
    };

    // ---- find the comparison block and exit ----------------------------
    //
    // Two cases:
    //   (a) 2-block loop: header IS the comparison block.
    //   (b) 4-block loop (canonical Fortran DO): header is a transparent
    //       relay block (0 instructions, br → cmp_block). cmp_block has
    //       the ICmp + CondBranch.
    //
    // In both cases we need (cmp_block, iv_bound, exit, exit_args).

    let (cmp_block, iv_bound, exit, exit_args) = {
        if let Some((bound, ex, args)) = detect_bound_and_exit(func, header, iv_param) {
            // Case (a): header has the comparison directly.
            (header, bound, ex, args)
        } else {
            // Case (b): header must be a transparent relay.
            let hdr_blk = func.block(header);
            if !hdr_blk.insts.is_empty() {
                return None;
            } // must have 0 instructions
            let relay_target = match &hdr_blk.terminator {
                Some(Terminator::Branch(t, args)) if args.is_empty() => *t,
                _ => return None,
            };
            if !nl.body.contains(&relay_target) {
                return None;
            }
            let (bound, ex, args) = detect_bound_and_exit(func, relay_target, iv_param)?;
            (relay_target, bound, ex, args)
        }
    };

    if nl.body.contains(&exit) {
        return None;
    }

    // ---- latch: stride-1 increment, passes iv (and optionally acc)
    //      back to header ------------------------------------------------
    let latch_info = check_latch(func, latch, header, iv_param, iv_param_idx, acc_param_idx)?;

    // ---- compute body_blocks: body − {header, cmp_block, latch} --------
    //
    // Walk from cmp_block's true-successor to latch (exclusive) in CFG
    // order. The sub-CFG must be a linear chain — any branch or divergence
    // means the loop has internal control flow we don't handle here.
    let body_blocks = collect_body_chain(func, cmp_block, latch, &nl.body)?;

    let is_do_concurrent = is_do_concurrent_loop(func, header, cmp_block, &body_blocks, latch);
    if !is_do_concurrent && body_blocks.iter().any(|&bb| block_contains_load(func, bb)) {
        return None;
    }

    // ---- no loop-defined values escape outside the loop ----------------
    //
    // We must check ALL blocks that will be pruned after unrolling — not just
    // body_blocks. In particular, `header` carries a block param (the IV) that
    // mem2reg may have threaded into OUTER loop latches via dominance-frontier
    // placement. If that param is used outside nl.body, the header's block param
    // becomes undefined after we discard the header block.
    for &bb in body_blocks
        .iter()
        .chain(std::iter::once(&header))
        .chain(std::iter::once(&cmp_block))
        .chain(std::iter::once(&latch))
    {
        if has_escaping_values(func, bb, &nl.body, preds) {
            return None;
        }
    }

    // ---- size check ------------------------------------------------------
    let total_insts: usize = body_blocks.iter().map(|&b| func.block(b).insts.len()).sum();
    if total_insts > BODY_SIZE_MAX {
        return None;
    }

    let full_unroll_max = if is_do_concurrent {
        DO_CONCURRENT_FULL_UNROLL_MAX
    } else {
        FULL_UNROLL_MAX
    };

    // ---- trip count ------------------------------------------------------
    let trip_count_i64 = iv_bound - iv_init + 1;
    if trip_count_i64 <= 0 {
        return None;
    }
    if trip_count_i64 > full_unroll_max {
        return None;
    }

    let reduction = match (acc_param, acc_init, latch_info.latch_acc_value) {
        (Some(p), Some(init), Some(latch_val)) => {
            // The latch accumulator value must be defined inside one
            // of the body_blocks — otherwise it lives in the latch
            // (which the unroller doesn't clone) and we'd drop the
            // reduction op. Bail in that case rather than miscompile.
            let defined_in_body = body_blocks
                .iter()
                .any(|&b| func.block(b).insts.iter().any(|i| i.id == latch_val));
            if !defined_in_body {
                return None;
            }
            Some(ReductionInfo {
                acc_param: p,
                acc_init: init,
                latch_acc_value: latch_val,
            })
        }
        (None, None, None) => None,
        // Inconsistent state — fall through and reject the loop. Should
        // not happen because the lane indices line up at the top.
        _ => return None,
    };

    Some(LoopShape {
        preheader,
        header,
        cmp_block,
        body_blocks,
        latch,
        exit,
        iv_param,
        iv_ty,
        iv_init,
        iv_bound,
        trip_count: trip_count_i64 as usize,
        exit_args,
        reduction,
    })
}

/// Walk from the comparison block's true-successor to the latch, collecting
/// the "body" blocks in order. Returns None if the sub-CFG is not a linear
/// chain (no branches to outside, no divergence).
///
/// Two structural cases:
///
/// (a) 2-block loop — the cmp_block's true-successor IS the latch. The latch
///     contains both the body instructions and the IV increment. We return
///     `[latch]` so the body is cloned from the latch's instructions.
///     The IV increment (`iadd iv, 1`) will be included in the clone but its
///     result is unused in the cloned block (the terminator is rewritten to
///     jump to the next iteration, discarding the iadd result). DCE cleans it.
///
/// (b) 4-block loop — cmp_block's true-successor is a separate body block
///     distinct from the latch. We walk the chain through the latch and
///     include it in the clone set because real lowered loops may still
///     carry side effects there in addition to the IV increment.
fn collect_body_chain(
    func: &Function,
    cmp_block: BlockId,
    latch: BlockId,
    loop_body: &HashSet<BlockId>,
) -> Option<Vec<BlockId>> {
    // Find the true-successor of cmp_block (the first body block).
    let first = match &func.block(cmp_block).terminator {
        Some(Terminator::CondBranch { true_dest, .. }) => *true_dest,
        _ => return None,
    };
    if !loop_body.contains(&first) {
        return None;
    }

    if first == latch {
        // Case (a): the cmp's true-successor is the latch itself.
        // The latch IS the body — include it in the clone chain.
        return Some(vec![latch]);
    }

    // Case (b): walk the chain from first through latch. The latch often
    // contains only the IV increment, but real fused/lowered loops can keep
    // user-visible side effects there too, so excluding it is unsound.
    let mut chain = Vec::new();
    let mut cur = first;
    loop {
        if !loop_body.contains(&cur) {
            return None;
        }
        chain.push(cur);
        if cur == latch {
            break;
        }
        let blk = func.block(cur);
        // Each block in the chain must have no params (no join point).
        if !blk.params.is_empty() {
            return None;
        }
        // Its terminator must be an unconditional branch to the next block.
        match &blk.terminator {
            Some(Terminator::Branch(next, args)) if args.is_empty() => {
                cur = *next;
            }
            _ => return None,
        }
        if chain.len() > 4 {
            return None;
        } // safety limit
    }
    Some(chain)
}

/// Delegate to shared loop utility.
fn resolve_const_int(func: &Function, vid: ValueId) -> Option<i64> {
    super::loop_utils::resolve_const_int(func, vid)
}

/// Inspect the header to find the loop bound and exit block.
///
/// Accepts:
///   - `ICmp(Sle, iv, const(hi))` → bound = hi, true_dest = latch, false_dest = exit
///   - `ICmp(Slt, iv, const(hi))` → bound = hi-1 (exclusive → inclusive), same
///   - `ICmp(Sge, const(hi), iv)` → bound = hi  (commuted)
///   - `ICmp(Sgt, const(hi), iv)` → bound = hi-1 (commuted exclusive)
///
/// Returns `(inclusive_bound, exit_block_id, exit_args)`.
fn detect_bound_and_exit(
    func: &Function,
    header: BlockId,
    iv: ValueId,
) -> Option<(i64, BlockId, Vec<ValueId>)> {
    let hdr = func.block(header);

    // Find the single ICmp that involves the IV.
    let mut cmp_id: Option<ValueId> = None;
    let mut bound: Option<i64> = None;
    let mut is_lt = false; // true when we need bound-1

    for inst in &hdr.insts {
        if let InstKind::ICmp(op, a, b) = &inst.kind {
            let other = if *a == iv {
                *b
            } else if *b == iv {
                *a
            } else {
                continue;
            };
            let c = resolve_const_int(func, other)?;
            match op {
                CmpOp::Le => {
                    bound = Some(c);
                    is_lt = false;
                }
                CmpOp::Lt => {
                    bound = Some(c);
                    is_lt = true;
                }
                CmpOp::Ge => {
                    bound = Some(c);
                    is_lt = false;
                } // iv >= c means upper = c when commuted
                CmpOp::Gt => {
                    bound = Some(c);
                    is_lt = true;
                } // iv > c means upper = c-1 commuted
                _ => return None,
            }
            cmp_id = Some(inst.id);
            break;
        }
    }
    let cmp_id = cmp_id?;
    let raw_bound = bound?;
    let inclusive_bound = if is_lt { raw_bound - 1 } else { raw_bound };

    // Terminator must be CondBranch on that cmp.
    match &hdr.terminator {
        Some(Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        }) if *cond == cmp_id => {
            // true → body (latch), false → exit.
            // Check which side is the latch. We don't have the latch id here;
            // we'll accept either ordering and return the "false" side as exit.
            // The caller verifies that the exit is not in the loop body.
            let _ = true_args;
            Some((inclusive_bound, *false_dest, false_args.clone()))
        }
        _ => None,
    }
}

/// Verify that the latch block increments the IV by 1 and passes only
/// the new IV value back to the header (stride-1 check).
///
/// Accepted pattern:
/// ```text
///   %i_next = iadd %iv, const(1)
///   br header(%i_next)
/// ```
/// Validate the latch terminator and (when applicable) capture the
/// reduction lane.
///
/// Accepts either:
///   * `br header(%iv_next)` — pure-IV loop, returns
///     `Some(LatchInfo { latch_acc_value: None })`.
///   * `br header(%iv_next, %new_acc)` — reduction loop, where
///     `acc_lane_idx` (passed by the caller as 0 or 1) tells us which
///     branch arg index carries the accumulator. Returns
///     `Some(LatchInfo { latch_acc_value: Some(%new_acc) })`.
///
/// In both cases the IV stride must be `iadd iv, const(1)` defined in
/// the latch.
fn check_latch(
    func: &Function,
    latch: BlockId,
    header: BlockId,
    iv: ValueId,
    iv_lane_idx: usize,
    acc_lane_idx: Option<usize>,
) -> Option<LatchInfo> {
    let blk = func.block(latch);

    let expected_args = if acc_lane_idx.is_some() { 2 } else { 1 };
    let args = match &blk.terminator {
        Some(Terminator::Branch(dest, args))
            if *dest == header && args.len() == expected_args =>
        {
            args.clone()
        }
        _ => return None,
    };

    let iv_next = args[iv_lane_idx];
    let acc_value = acc_lane_idx.map(|i| args[i]);

    // The iv lane's value must be defined as `iadd %iv, const(1)`
    // inside the latch.
    let mut iv_ok = false;
    for inst in &blk.insts {
        if inst.id == iv_next {
            if let InstKind::IAdd(a, b) = &inst.kind {
                let (is_iv, other) = if *a == iv {
                    (true, *b)
                } else if *b == iv {
                    (true, *a)
                } else {
                    (false, *a)
                };
                if !is_iv {
                    return None;
                }
                if resolve_const_int(func, other) != Some(1) {
                    return None;
                }
                iv_ok = true;
            } else {
                return None;
            }
            break;
        }
    }
    if !iv_ok {
        return None;
    }

    Some(LatchInfo {
        latch_acc_value: acc_value,
    })
}

#[derive(Debug, Clone, Copy)]
struct LatchInfo {
    latch_acc_value: Option<ValueId>,
}

/// Return true if any instruction result defined in the latch is used
/// in a block that is NOT part of the loop body.
///
/// This ensures no latch-computed value leaks out into the exit or
/// subsequent blocks. (Such loops have loop-carried dependencies that
/// require different handling.)
fn has_escaping_values(
    func: &Function,
    latch: BlockId,
    body: &HashSet<BlockId>,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> bool {
    // Collect all ValueIds defined in the block — both block params and
    // instruction results. Block params matter because mem2reg places them at
    // dominance-frontier blocks; if such a param escapes into an outer-loop
    // latch it becomes undefined after unrolling removes the block.
    let latch_blk = func.block(latch);
    let mut latch_defs: HashSet<ValueId> = HashSet::new();
    for bp in &latch_blk.params {
        latch_defs.insert(bp.id);
    }
    for inst in &latch_blk.insts {
        latch_defs.insert(inst.id);
    }

    if latch_defs.is_empty() {
        return false;
    }

    // Check every block outside the loop body for uses of those values.
    for block in &func.blocks {
        if body.contains(&block.id) {
            continue;
        }
        // Check instruction operands.
        for inst in &block.insts {
            for use_id in inst_uses(&inst.kind) {
                if latch_defs.contains(&use_id) {
                    return true;
                }
            }
        }
        // Check terminator operands.
        if let Some(term) = &block.terminator {
            for &use_id in terminator_uses_vec(term).iter() {
                if latch_defs.contains(&use_id) {
                    return true;
                }
            }
        }
        // Also check block params of outside blocks that receive args from latch.
        // (If the latch branches to an outside block with args, those args
        // might carry latch values. But `check_latch` already restricts the
        // latch terminator to `br header(%iv_next)`, so this can't happen for
        // loops we accept. Kept as belt-and-suspenders.)
    }
    let _ = preds;
    false
}

fn block_contains_load(func: &Function, block: BlockId) -> bool {
    func.block(block)
        .insts
        .iter()
        .any(|inst| matches!(inst.kind, InstKind::Load(_)))
}

/// Collect all ValueId operands in a terminator into a Vec.
/// (Mirrors `terminator_uses` from walk.rs but returns owned storage.)
fn terminator_uses_vec(term: &Terminator) -> Vec<ValueId> {
    let mut out = Vec::new();
    match term {
        Terminator::Return(Some(v)) => out.push(*v),
        Terminator::Branch(_, args) => out.extend(args),
        Terminator::CondBranch {
            cond,
            true_args,
            false_args,
            ..
        } => {
            out.push(*cond);
            out.extend(true_args);
            out.extend(false_args);
        }
        Terminator::Switch { selector, .. } => out.push(*selector),
        _ => {}
    }
    out
}

/// Import inst_uses from the walk module for convenience.
fn inst_uses(kind: &InstKind) -> Vec<ValueId> {
    crate::ir::walk::inst_uses(kind)
}

// ---------------------------------------------------------------------------
// Transformation
// ---------------------------------------------------------------------------

fn do_unroll(func: &mut Function, shape: LoopShape) {
    // Collect per-body-block instructions once (before we mutate func).
    let body_snapshots: Vec<Vec<Inst>> = shape
        .body_blocks
        .iter()
        .map(|&b| func.block(b).insts.clone())
        .collect();

    // Determine the IV width.
    let iv_width = match &shape.iv_ty {
        IrType::Int(w) => *w,
        _ => IntWidth::I32,
    };

    // For each iteration, we create one new block per body block, cloned
    // with the IV substituted by a constant. The chain is:
    //
    //   preheader → iter0_body0 → iter0_body1 → ... → iter0_bodyN-1
    //             → iter1_body0 → ...
    //             → iter{TC-1}_body0 → ...
    //             → exit
    //
    // If body_blocks is empty (cmp_block directly targets latch), the
    // iterations have no instructions and we just wire preheader → exit.

    // Accumulate the block ids for each iteration so we can wire terminators.
    let tc = shape.trip_count;
    let nb = body_snapshots.len();

    let mut iter_blocks: Vec<Vec<BlockId>> = Vec::with_capacity(tc);
    let mut iter_substs: Vec<HashMap<ValueId, ValueId>> = Vec::with_capacity(tc);

    // Reduction threading: prev_acc carries iteration k-1's latch
    // accumulator value into iteration k's body substitution. For
    // iteration 0 it's seeded from the preheader's branch arg; after
    // each iteration completes, it's updated to that iter's body
    // output (looked up through the iter's subst by the original
    // latch_acc_value's ID).
    let mut prev_acc: Option<ValueId> = shape.reduction.as_ref().map(|r| r.acc_init);

    for k in 0..tc {
        let iv_val = shape.iv_init + k as i64;

        // Build substitution map seeded with iv_param → const(iv_val)
        // and (when present) acc_param → previous iteration's latch
        // accumulator value.
        let mut subst: HashMap<ValueId, ValueId> = HashMap::new();

        // Emit the IV constant (will be placed in each new block's first inst).
        let iv_const_id = func.next_value_id();
        subst.insert(shape.iv_param, iv_const_id);

        if let (Some(r), Some(prev)) = (shape.reduction.as_ref(), prev_acc) {
            subst.insert(r.acc_param, prev);
        }

        let mut blk_ids = Vec::with_capacity(nb.max(1));
        if nb == 0 {
            // No body blocks — create a single pass-through block.
            let bid = func.create_block(&format!("unroll_k{}", k));
            blk_ids.push(bid);
        } else {
            for (bi, snap) in body_snapshots.iter().enumerate() {
                let bid = func.create_block(&format!("unroll_k{}b{}", k, bi));
                // First block of this iter: emit IV constant.
                if bi == 0 {
                    let iv_inst = Inst {
                        id: iv_const_id,
                        kind: InstKind::ConstInt(iv_val as i128, iv_width),
                        ty: shape.iv_ty.clone(),
                        span: dummy_span(),
                    };
                    func.block_mut(bid).insts.push(iv_inst);
                }
                // Clone body instructions.
                for inst in snap {
                    let new_id = func.next_value_id();
                    subst.insert(inst.id, new_id);
                    let new_kind = remap_kind(&inst.kind, &subst);
                    func.block_mut(bid).insts.push(Inst {
                        id: new_id,
                        kind: new_kind,
                        ty: inst.ty.clone(),
                        span: inst.span,
                    });
                }
                blk_ids.push(bid);
            }
        }
        // After this iteration's body has been cloned, look up the
        // post-substitution form of the latch's accumulator value —
        // that becomes the seed for the next iteration's acc_param
        // substitution. If the latch's acc value wasn't defined in
        // this body (e.g. an invariant or pre-loop value), fall back
        // to the original ID; the unroller's other safety checks
        // ensure that's still well-defined post-pruning.
        if let Some(r) = shape.reduction.as_ref() {
            prev_acc = Some(
                subst
                    .get(&r.latch_acc_value)
                    .copied()
                    .unwrap_or(r.latch_acc_value),
            );
        }
        iter_blocks.push(blk_ids);
        iter_substs.push(subst);
    }

    // Wire terminators within each iteration (intra-iter body chain)
    // and across iterations (last body block of iter k → first of iter k+1).
    let original_body_next: Vec<BlockId> = {
        // For each body block b, what block does it branch to?
        // (It's either the next body block or the latch.)
        let mut nexts = Vec::new();
        for (bi, &b) in shape.body_blocks.iter().enumerate() {
            let next = if bi + 1 < shape.body_blocks.len() {
                shape.body_blocks[bi + 1]
            } else {
                shape.latch
            };
            let _ = b;
            nexts.push(next);
        }
        nexts
    };

    for k in 0..tc {
        let nb_actual = iter_blocks[k].len();
        // Determine which block this iteration chains to after its last body block.
        let after_iter = if k + 1 < tc {
            // First block of next iteration.
            iter_blocks[k + 1][0]
        } else {
            // Last iteration → exit.
            shape.exit
        };

        // Args to pass when branching to the exit block. For
        // reduction loops, the original `exit_args` references the
        // header's acc_param — pruned along with the rest of the
        // header. Replace the acc_param entry with the final
        // iteration's accumulator value (`prev_acc`, set up after
        // the per-iter cloning loop ran iteration N-1). Other
        // entries are left alone, falling through to the verifier
        // if they reference values that would survive (today's only
        // supported shape has exactly the acc lane).
        let exit_args = if after_iter == shape.exit {
            if let (Some(r), Some(final_acc)) = (shape.reduction.as_ref(), prev_acc) {
                shape
                    .exit_args
                    .iter()
                    .map(|&v| if v == r.acc_param { final_acc } else { v })
                    .collect()
            } else {
                shape.exit_args.clone()
            }
        } else {
            vec![]
        };

        if nb == 0 {
            // No body — the single pass-through block jumps to after_iter.
            func.block_mut(iter_blocks[k][0]).terminator =
                Some(Terminator::Branch(after_iter, exit_args));
        } else {
            for bi in 0..nb_actual {
                let cur_blk = iter_blocks[k][bi];
                let (next_blk, args) = if bi + 1 < nb_actual {
                    (iter_blocks[k][bi + 1], vec![])
                } else {
                    // Last body block of iter k → after_iter.
                    (after_iter, exit_args.clone())
                };
                let _ = original_body_next[bi]; // consumed above
                func.block_mut(cur_blk).terminator = Some(Terminator::Branch(next_blk, args));
            }
        }
    }

    // Rewrite preheader terminator: skip the loop header, jump to first iter.
    let (first_block, preheader_args) = if tc > 0 {
        (iter_blocks[0][0], vec![])
    } else {
        // Zero-trip loop: jump directly to exit with its required args.
        (shape.exit, shape.exit_args.clone())
    };
    func.block_mut(shape.preheader).terminator =
        Some(Terminator::Branch(first_block, preheader_args));

    // Mark loop control blocks as Unreachable → prune_unreachable removes them.
    func.block_mut(shape.header).terminator = Some(Terminator::Unreachable);
    func.block_mut(shape.cmp_block).terminator = Some(Terminator::Unreachable);
    func.block_mut(shape.latch).terminator = Some(Terminator::Unreachable);
    for &b in &shape.body_blocks {
        func.block_mut(b).terminator = Some(Terminator::Unreachable);
    }
}

/// Deep-clone an InstKind, substituting all ValueId operands according to `subst`.
fn remap_kind(kind: &InstKind, subst: &HashMap<ValueId, ValueId>) -> InstKind {
    let r = |v: &ValueId| *subst.get(v).unwrap_or(v);
    match kind {
        // Constants — no operands.
        InstKind::ConstInt(v, w) => InstKind::ConstInt(*v, *w),
        InstKind::ConstFloat(v, w) => InstKind::ConstFloat(*v, *w),
        InstKind::ConstBool(v) => InstKind::ConstBool(*v),
        InstKind::ConstString(v) => InstKind::ConstString(v.clone()),
        InstKind::Undef(t) => InstKind::Undef(t.clone()),
        InstKind::GlobalAddr(s) => InstKind::GlobalAddr(s.clone()),

        // Integer arithmetic.
        InstKind::IAdd(a, b) => InstKind::IAdd(r(a), r(b)),
        InstKind::ISub(a, b) => InstKind::ISub(r(a), r(b)),
        InstKind::IMul(a, b) => InstKind::IMul(r(a), r(b)),
        InstKind::IDiv(a, b) => InstKind::IDiv(r(a), r(b)),
        InstKind::IMod(a, b) => InstKind::IMod(r(a), r(b)),
        InstKind::INeg(a) => InstKind::INeg(r(a)),

        // Float arithmetic.
        InstKind::FAdd(a, b) => InstKind::FAdd(r(a), r(b)),
        InstKind::FSub(a, b) => InstKind::FSub(r(a), r(b)),
        InstKind::FMul(a, b) => InstKind::FMul(r(a), r(b)),
        InstKind::FDiv(a, b) => InstKind::FDiv(r(a), r(b)),
        InstKind::FNeg(a) => InstKind::FNeg(r(a)),
        InstKind::FAbs(a) => InstKind::FAbs(r(a)),
        InstKind::FSqrt(a) => InstKind::FSqrt(r(a)),
        InstKind::FPow(a, b) => InstKind::FPow(r(a), r(b)),

        // Comparison.
        InstKind::ICmp(op, a, b) => InstKind::ICmp(*op, r(a), r(b)),
        InstKind::FCmp(op, a, b) => InstKind::FCmp(*op, r(a), r(b)),

        // Logic.
        InstKind::And(a, b) => InstKind::And(r(a), r(b)),
        InstKind::Or(a, b) => InstKind::Or(r(a), r(b)),
        InstKind::Not(a) => InstKind::Not(r(a)),

        // Select.
        InstKind::Select(c, t, f) => InstKind::Select(r(c), r(t), r(f)),

        // Bitwise.
        InstKind::BitAnd(a, b) => InstKind::BitAnd(r(a), r(b)),
        InstKind::BitOr(a, b) => InstKind::BitOr(r(a), r(b)),
        InstKind::BitXor(a, b) => InstKind::BitXor(r(a), r(b)),
        InstKind::BitNot(a) => InstKind::BitNot(r(a)),
        InstKind::Shl(a, b) => InstKind::Shl(r(a), r(b)),
        InstKind::LShr(a, b) => InstKind::LShr(r(a), r(b)),
        InstKind::AShr(a, b) => InstKind::AShr(r(a), r(b)),
        InstKind::CountLeadingZeros(a) => InstKind::CountLeadingZeros(r(a)),
        InstKind::CountTrailingZeros(a) => InstKind::CountTrailingZeros(r(a)),
        InstKind::PopCount(a) => InstKind::PopCount(r(a)),

        // Conversions.
        InstKind::IntToFloat(a, w) => InstKind::IntToFloat(r(a), *w),
        InstKind::FloatToInt(a, w) => InstKind::FloatToInt(r(a), *w),
        InstKind::FloatExtend(a, w) => InstKind::FloatExtend(r(a), *w),
        InstKind::FloatTrunc(a, w) => InstKind::FloatTrunc(r(a), *w),
        InstKind::IntExtend(a, w, s) => InstKind::IntExtend(r(a), *w, *s),
        InstKind::IntTrunc(a, w) => InstKind::IntTrunc(r(a), *w),
        InstKind::PtrToInt(a) => InstKind::PtrToInt(r(a)),
        InstKind::IntToPtr(a, ty) => InstKind::IntToPtr(r(a), ty.clone()),

        // Memory.
        InstKind::Alloca(t) => InstKind::Alloca(t.clone()),
        InstKind::Load(a) => InstKind::Load(r(a)),
        InstKind::Store(v, p) => InstKind::Store(r(v), r(p)),
        InstKind::GetElementPtr(base, idxs) => {
            InstKind::GetElementPtr(r(base), idxs.iter().map(&r).collect())
        }

        // Calls.
        InstKind::Call(f, args) => InstKind::Call(f.clone(), args.iter().map(&r).collect()),
        InstKind::RuntimeCall(f, args) => {
            InstKind::RuntimeCall(f.clone(), args.iter().map(&r).collect())
        }

        // Aggregates.
        InstKind::ExtractField(v, idx) => InstKind::ExtractField(r(v), *idx),
        InstKind::InsertField(v, idx, fld) => InstKind::InsertField(r(v), *idx, r(fld)),

        // ---- SIMD vector ops ----
        InstKind::VAdd(a, b) => InstKind::VAdd(r(a), r(b)),
        InstKind::VSub(a, b) => InstKind::VSub(r(a), r(b)),
        InstKind::VMul(a, b) => InstKind::VMul(r(a), r(b)),
        InstKind::VDiv(a, b) => InstKind::VDiv(r(a), r(b)),
        InstKind::VNeg(a) => InstKind::VNeg(r(a)),
        InstKind::VAbs(a) => InstKind::VAbs(r(a)),
        InstKind::VSqrt(a) => InstKind::VSqrt(r(a)),
        InstKind::VFma(a, b, c) => InstKind::VFma(r(a), r(b), r(c)),
        InstKind::VSelect(m, t, f) => InstKind::VSelect(r(m), r(t), r(f)),
        InstKind::VMin(a, b) => InstKind::VMin(r(a), r(b)),
        InstKind::VMax(a, b) => InstKind::VMax(r(a), r(b)),
        InstKind::VICmp(op, a, b) => InstKind::VICmp(*op, r(a), r(b)),
        InstKind::VFCmp(op, a, b) => InstKind::VFCmp(*op, r(a), r(b)),
        InstKind::VLoad(p) => InstKind::VLoad(r(p)),
        InstKind::VStore(v, p) => InstKind::VStore(r(v), r(p)),
        InstKind::VBitcast(v, ty) => InstKind::VBitcast(r(v), ty.clone()),
        InstKind::VExtract(v, lane) => InstKind::VExtract(r(v), *lane),
        InstKind::VInsert(v, lane, s) => InstKind::VInsert(r(v), *lane, r(s)),
        InstKind::VBroadcast(s) => InstKind::VBroadcast(r(s)),
        InstKind::VReduceSum(v) => InstKind::VReduceSum(r(v)),
        InstKind::VReduceMin(v) => InstKind::VReduceMin(r(v)),
        InstKind::VReduceMax(v) => InstKind::VReduceMax(r(v)),
    }
}

// ---------------------------------------------------------------------------
// Partial unrolling
// ---------------------------------------------------------------------------
//
// Loops with statically known trip counts above FULL_UNROLL_MAX don't
// benefit from full unrolling (code bloat), but a U-way partial unroll
// (clone body U-1 additional times, step IV by U) can still expose ILP
// and reduce loop overhead. v0 handles the simplest shape:
//
//   * 2-block loop (header + latch).
//   * Single block param (no reduction).
//   * Static trip count > FULL_UNROLL_MAX, divisible by `U`.
//   * Body instruction count × U ≤ PARTIAL_UNROLL_BODY_BUDGET.
//
// The transform clones the latch's body insts U-1 more times with
// `iv` substituted by a freshly-computed `iv + k` (1 ≤ k < U), then
// rewrites the latch's `iadd iv, 1` to `iadd iv, U`.

/// Largest unrolled-body size in instructions for partial unrolling.
const PARTIAL_UNROLL_BODY_BUDGET: usize = 60;
/// Largest unroll factor we'll apply.
const PARTIAL_UNROLL_MAX_FACTOR: usize = 4;

#[derive(Debug)]
struct PartialShape {
    header: BlockId,
    latch: BlockId,
    iv_param: ValueId,
    iv_ty: IrType,
    iv_init: i64,
    iv_bound: i64,
    /// Unroll factor.
    u: usize,
    /// The latch's `iadd iv, 1` instruction id.
    iadd_id: ValueId,
    /// The const(1) value feeding the iadd.
    step_const_id: ValueId,
    /// If the loop is a reduction (header has 2 params), this carries
    /// the acc param id and the latch's terminator's new-acc value.
    reduction: Option<PartialReductionInfo>,
}

#[derive(Debug, Clone, Copy)]
struct PartialReductionInfo {
    acc_param: ValueId,
    /// The value the latch passes back as the new accumulator
    /// (terminator's args[1]).
    new_acc: ValueId,
}

fn detect_partial_unroll_loop(
    func: &Function,
    nl: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<PartialShape> {
    if nl.latches.len() != 1 || nl.body.len() != 2 {
        return None;
    }
    let header = nl.header;
    let latch = nl.latches[0];
    if header == latch {
        return None;
    }
    let hdr = func.block(header);
    if hdr.params.is_empty() || hdr.params.len() > 2 {
        return None;
    }
    let iv_param = hdr.params[0].id;
    let iv_ty = hdr.params[0].ty.clone();
    if !matches!(iv_ty, IrType::Int(_)) {
        return None;
    }
    let acc_param: Option<ValueId> = if hdr.params.len() == 2 {
        Some(hdr.params[1].id)
    } else {
        None
    };
    // Header has the cmp + cond_br body/exit.
    let (iv_bound, exit, _exit_args) = detect_bound_and_exit(func, header, iv_param)?;
    if nl.body.contains(&exit) {
        return None;
    }
    // Preheader supplies iv_init.
    let header_preds = preds.get(&header)?;
    let mut outside: Vec<BlockId> = header_preds
        .iter()
        .copied()
        .filter(|p| !nl.body.contains(p))
        .collect();
    outside.sort_by_key(|b| b.0);
    outside.dedup();
    if outside.len() != 1 {
        return None;
    }
    let preheader = outside[0];
    let ph = func.block(preheader);
    let expected_args = if acc_param.is_some() { 2 } else { 1 };
    let iv_init = match &ph.terminator {
        Some(Terminator::Branch(d, args))
            if *d == header && args.len() == expected_args =>
        {
            resolve_const_int(func, args[0])?
        }
        _ => return None,
    };
    // Latch: `... insts ...; iadd iv, 1; br header(iv_next [, new_acc])`.
    let latch_blk = func.block(latch);
    let latch_term_args = match &latch_blk.terminator {
        Some(Terminator::Branch(d, args))
            if *d == header && args.len() == expected_args =>
        {
            args.clone()
        }
        _ => return None,
    };
    let iv_next = latch_term_args[0];
    let new_acc = if acc_param.is_some() {
        Some(latch_term_args[1])
    } else {
        None
    };
    let iadd_inst = latch_blk.insts.iter().find(|i| i.id == iv_next)?;
    let (lhs, rhs) = match iadd_inst.kind {
        InstKind::IAdd(l, r) => (l, r),
        _ => return None,
    };
    let step_const_id = if lhs == iv_param {
        rhs
    } else if rhs == iv_param {
        lhs
    } else {
        return None;
    };
    if resolve_const_int(func, step_const_id) != Some(1) {
        return None;
    }
    // No latch-defined value escapes the loop.
    let body_set: HashSet<BlockId> = nl.body.iter().copied().collect();
    if has_escaping_values(func, latch, &body_set, preds) {
        return None;
    }
    // Trip count gate: > FULL_UNROLL_MAX (full unroller would have
    // taken it otherwise) and divisible by some U in [2, MAX_FACTOR].
    let trip = iv_bound - iv_init + 1;
    if trip <= FULL_UNROLL_MAX {
        return None;
    }
    let body_inst_count = latch_blk.insts.len();
    if body_inst_count == 0 {
        return None;
    }
    // Pick the largest U ≤ MAX_FACTOR that divides trip and respects
    // the body-size budget.
    let mut chosen_u: Option<usize> = None;
    for u in (2..=PARTIAL_UNROLL_MAX_FACTOR).rev() {
        if trip % (u as i64) != 0 {
            continue;
        }
        if body_inst_count * u > PARTIAL_UNROLL_BODY_BUDGET {
            continue;
        }
        chosen_u = Some(u);
        break;
    }
    let u = chosen_u?;
    let reduction = match (acc_param, new_acc) {
        (Some(p), Some(v)) => Some(PartialReductionInfo {
            acc_param: p,
            new_acc: v,
        }),
        (None, None) => None,
        _ => return None,
    };
    Some(PartialShape {
        header,
        latch,
        iv_param,
        iv_ty,
        iv_init,
        iv_bound,
        u,
        iadd_id: iv_next,
        step_const_id,
        reduction,
    })
}

fn do_partial_unroll(func: &mut Function, shape: PartialShape) {
    // Snapshot of latch body BEFORE the iadd. We'll clone these
    // instructions U-1 more times with iv substituted.
    let latch_snapshot: Vec<Inst> = func.block(shape.latch).insts.clone();
    // Precompute body-without-iadd for cloning.
    let body_inst_clone: Vec<Inst> = latch_snapshot
        .iter()
        .filter(|i| i.id != shape.iadd_id)
        .cloned()
        .collect();
    let iv_width = match shape.iv_ty {
        IrType::Int(w) => w,
        _ => return,
    };
    let span = dummy_span();
    // For each k in 1..U: emit `iv_k = iadd iv, k_const`, then clone
    // every body inst (except iadd) with iv → iv_k. Append all of
    // these BEFORE the existing iadd, so the sequence is:
    //   <orig body insts>          ; uses iv
    //   iv_1 = iadd iv, 1
    //   <body cloned, iv → iv_1>
    //   iv_2 = iadd iv, 2
    //   <body cloned, iv → iv_2>
    //   ...
    //   iv_next = iadd iv, U
    //   br header(iv_next)
    //
    // We construct the new instruction list off-band, then swap
    // it in.
    let mut new_insts: Vec<Inst> = latch_snapshot
        .iter()
        .filter(|i| i.id != shape.iadd_id)
        .cloned()
        .collect();
    // For reduction loops, track the running accumulator. After the
    // original (k=0) body, the running acc is the original new_acc.
    // Each subsequent clone substitutes acc_param → prev_acc and
    // produces a fresh new_acc (the cloned counterpart of orig new_acc).
    let mut prev_acc: Option<ValueId> = shape.reduction.map(|r| r.new_acc);
    for k in 1..shape.u {
        // Allocate the const(k) and the iv_k = iadd iv, k.
        let k_const_id = func.next_value_id();
        func.register_type(k_const_id, shape.iv_ty.clone());
        let iv_k_id = func.next_value_id();
        func.register_type(iv_k_id, shape.iv_ty.clone());
        new_insts.push(Inst {
            id: k_const_id,
            kind: InstKind::ConstInt(k as i128, iv_width),
            ty: shape.iv_ty.clone(),
            span,
        });
        new_insts.push(Inst {
            id: iv_k_id,
            kind: InstKind::IAdd(shape.iv_param, k_const_id),
            ty: shape.iv_ty.clone(),
            span,
        });
        // Clone the body (sans iadd) substituting iv → iv_k and
        // (for reductions) acc_param → prev_acc; each inst id →
        // fresh id.
        let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
        subst.insert(shape.iv_param, iv_k_id);
        if let (Some(r), Some(prev)) = (shape.reduction, prev_acc) {
            subst.insert(r.acc_param, prev);
        }
        for orig in &body_inst_clone {
            let new_id = func.next_value_id();
            func.register_type(new_id, orig.ty.clone());
            subst.insert(orig.id, new_id);
            let new_kind = remap_kind(&orig.kind, &subst);
            new_insts.push(Inst {
                id: new_id,
                kind: new_kind,
                ty: orig.ty.clone(),
                span: orig.span,
            });
        }
        // Walk forward the accumulator: prev_acc becomes the cloned
        // counterpart of orig.new_acc.
        if let Some(r) = shape.reduction {
            prev_acc = subst.get(&r.new_acc).copied();
        }
    }
    // Finally, the new iadd: iv_next = iadd iv, U.
    let new_step_const = func.next_value_id();
    func.register_type(new_step_const, shape.iv_ty.clone());
    new_insts.push(Inst {
        id: new_step_const,
        kind: InstKind::ConstInt(shape.u as i128, iv_width),
        ty: shape.iv_ty.clone(),
        span,
    });
    // Replace iadd's step constant. Reuse the existing iadd id so
    // the latch terminator (which references it) stays valid.
    let new_iadd = Inst {
        id: shape.iadd_id,
        kind: InstKind::IAdd(shape.iv_param, new_step_const),
        ty: shape.iv_ty.clone(),
        span,
    };
    new_insts.push(new_iadd);

    // Swap the latch's instruction list.
    func.block_mut(shape.latch).insts = new_insts;

    // For reduction loops, retarget the latch terminator's args[1]
    // to the FINAL accumulator (after U-1 clones).
    if let (Some(_r), Some(final_acc)) = (shape.reduction, prev_acc) {
        let latch_blk = func.block_mut(shape.latch);
        if let Some(Terminator::Branch(_, args)) = &mut latch_blk.terminator {
            if args.len() == 2 {
                args[1] = final_acc;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::Span;
    use crate::opt::pass::Pass;

    fn span() -> Span {
        super::dummy_span()
    }

    /// Build a minimal Module with one function containing a simple
    /// counted loop: do i = 1, N; a(i_offset) = const(42); end do
    ///
    /// The loop has 4 blocks: entry (preheader), header, latch, exit.
    /// The latch stores a constant to a fixed alloca (simulating a(i) = 42
    /// where the GEP is not yet computed — we just store to a fixed addr).
    fn build_counted_loop_with_prefix(lo: i64, hi: i64, prefix: &str) -> Module {
        let mut m = Module::new("test".into());
        let mut f = Function::new("loop_test".into(), vec![], IrType::Void);

        // Allocate block IDs.
        let header_id = f.create_block(&format!("{}_check", prefix));
        let latch_id = f.create_block(&format!("{}_body", prefix));
        let exit_id = f.create_block("exit");
        let entry_id = f.entry; // preheader

        // ---- entry (preheader) -------------------------------------------
        // const lo
        let lo_val = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: lo_val,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(lo as i128, IntWidth::I64),
        });
        // alloca for the "array" (just a slot we store into)
        let alloca_val = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: alloca_val,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I64)),
        });
        // br header(lo)
        f.block_mut(entry_id).terminator = Some(Terminator::Branch(header_id, vec![lo_val]));

        // ---- header (%i) -------------------------------------------------
        let iv_param = f.next_value_id();
        f.block_mut(header_id).params.push(BlockParam {
            id: iv_param,
            ty: IrType::Int(IntWidth::I64),
        });
        // const hi
        let hi_val = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: hi_val,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(hi as i128, IntWidth::I64),
        });
        // %cmp = icmp.sle %i, hi
        let cmp_val = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: cmp_val,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv_param, hi_val),
        });
        // condBr %cmp, latch, exit
        f.block_mut(header_id).terminator = Some(Terminator::CondBranch {
            cond: cmp_val,
            true_dest: latch_id,
            true_args: vec![],
            false_dest: exit_id,
            false_args: vec![],
        });

        // ---- latch -------------------------------------------------------
        // Store 42 to the alloca (simulates a(i) = 42).
        let const42 = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: const42,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(42, IntWidth::I64),
        });
        let store_id = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: store_id,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(const42, alloca_val),
        });
        // %i_next = iadd %i, 1
        let one = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: one,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let i_next = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: i_next,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::IAdd(iv_param, one),
        });
        // br header(%i_next)
        f.block_mut(latch_id).terminator = Some(Terminator::Branch(header_id, vec![i_next]));

        // ---- exit --------------------------------------------------------
        f.block_mut(exit_id).terminator = Some(Terminator::Return(None));

        m.add_function(f);
        m
    }

    fn build_counted_loop(lo: i64, hi: i64) -> Module {
        build_counted_loop_with_prefix(lo, hi, "do")
    }

    #[test]
    fn unrolls_trip4() {
        let mut m = build_counted_loop(1, 4);
        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(changed, "expected unroll to fire");

        let f = &m.functions[0];
        // After unrolling: preheader, 4 cloned iteration blocks, exit = 6 blocks.
        // The original header and latch are pruned (marked Unreachable then removed).
        assert_eq!(f.blocks.len(), 6, "expected entry + 4 iter blocks + exit");

        // Each iteration block has a ConstInt for that iteration's IV value.
        let all_iv_consts: Vec<i128> = f
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter_map(|i| {
                if let InstKind::ConstInt(v, _) = i.kind {
                    Some(v)
                } else {
                    None
                }
            })
            .collect();
        assert!(all_iv_consts.contains(&1), "IV=1 should appear");
        assert!(all_iv_consts.contains(&2), "IV=2 should appear");
        assert!(all_iv_consts.contains(&3), "IV=3 should appear");
        assert!(all_iv_consts.contains(&4), "IV=4 should appear");
    }

    #[test]
    fn partial_unrolls_trip10() {
        // Trip 10 > FULL_UNROLL_MAX=8 — full unroll skips it, but
        // partial unroll picks U=2 (largest factor of 10 that fits
        // the body budget) and doubles the body in place.
        let mut m = build_counted_loop(1, 10);
        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(changed, "trip-10 loop should partial-unroll");
        // Same number of blocks (loop is preserved); body inst count
        // grew because we cloned the body once with iv → iv+1.
        let f = &m.functions[0];
        let block_count = f.blocks.len();
        assert!(
            block_count >= 3,
            "expected loop structure preserved, got {} blocks",
            block_count
        );
    }

    #[test]
    fn does_not_partial_unroll_trip_above_threshold_with_no_factor() {
        // Trip 11 is prime and > FULL_UNROLL_MAX — neither full nor
        // partial unroll fires.
        let mut m = build_counted_loop(1, 11);
        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(!changed, "trip-11 (prime) loop should not unroll");
    }

    #[test]
    fn unrolls_trip10_do_concurrent() {
        let mut m = build_counted_loop_with_prefix(1, 10, "doconc");
        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(
            changed,
            "DO CONCURRENT trip-count-10 loop should fully unroll"
        );

        let f = &m.functions[0];
        assert_eq!(f.blocks.len(), 12, "expected entry + 10 iter blocks + exit");
    }

    #[test]
    fn does_not_unroll_empty_loop() {
        let mut m = build_counted_loop(5, 3); // lo > hi → trip count 0
        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(!changed, "empty loop should be left for DCE");
    }

    #[test]
    fn unrolls_reduction_loop_threading_accumulator() {
        // A reduction loop has 2 block params (iv + accumulator).
        // The unroller should clone the body once per iteration and
        // thread the accumulator value across iterations: iter 0
        // reads `acc_init`, iter 1 reads iter 0's `new_acc`, etc. The
        // final branch to `exit` passes the last iteration's
        // accumulator value to the exit block param.
        let mut m = Module::new("test".into());
        let mut f = Function::new("reduce".into(), vec![], IrType::Void);
        let header_id = f.create_block("header");
        let latch_id = f.create_block("latch");
        let exit_id = f.create_block("exit");
        let entry_id = f.entry;

        // Entry: br header(1, 0)
        let lo = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: lo,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let z = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: z,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(0, IntWidth::I64),
        });
        f.block_mut(entry_id).terminator = Some(Terminator::Branch(header_id, vec![lo, z]));

        // Header: 2 params (iv, acc)
        let iv = f.next_value_id();
        let acc = f.next_value_id();
        f.block_mut(header_id).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I64),
        });
        f.block_mut(header_id).params.push(BlockParam {
            id: acc,
            ty: IrType::Int(IntWidth::I64),
        });
        let hi = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: hi,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(4, IntWidth::I64),
        });
        let cmp = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: cmp,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, hi),
        });
        f.block_mut(header_id).terminator = Some(Terminator::CondBranch {
            cond: cmp,
            true_dest: latch_id,
            true_args: vec![],
            false_dest: exit_id,
            false_args: vec![acc],
        });

        // Latch: acc_next = acc + iv; i_next = iv + 1; br header(i_next, acc_next)
        let one = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: one,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let acc_next = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: acc_next,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::IAdd(acc, iv),
        });
        let i_next = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: i_next,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::IAdd(iv, one),
        });
        f.block_mut(latch_id).terminator =
            Some(Terminator::Branch(header_id, vec![i_next, acc_next]));

        // Exit: acc param, return
        let acc_out = f.next_value_id();
        f.block_mut(exit_id).params.push(BlockParam {
            id: acc_out,
            ty: IrType::Int(IntWidth::I64),
        });
        f.block_mut(exit_id).terminator = Some(Terminator::Return(None));

        m.add_function(f);

        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(changed, "reduction loop should now unroll");

        let f = &m.functions[0];

        // The exit block must still exist and its (only) predecessor
        // path now feeds it a non-acc-param value — i.e. a value
        // defined in one of the cloned iteration blocks.
        let exit_blk = f
            .blocks
            .iter()
            .find(|b| b.id == exit_id)
            .expect("exit survives");
        assert_eq!(exit_blk.params.len(), 1, "exit still has its acc_out param");

        // Find the predecessor block whose terminator branches to the
        // exit block. Its single arg should be the unrolled chain's
        // final accumulator value, NOT the original `acc` header
        // param (which is now unreachable).
        let mut found_branch_to_exit = false;
        for blk in &f.blocks {
            if let Some(Terminator::Branch(dest, args)) = &blk.terminator {
                if *dest == exit_id {
                    found_branch_to_exit = true;
                    assert_eq!(args.len(), 1, "exit takes one arg (the accumulator)");
                    assert_ne!(args[0], acc, "must not reference the pruned header acc param");
                    assert_ne!(args[0], iv, "must not reference the pruned header iv param");
                    break;
                }
            }
        }
        assert!(
            found_branch_to_exit,
            "must find an unrolled iter block branching to the original exit"
        );

        // Trip count = 4 → expect 4 cloned IAdd-of-acc instructions in
        // the function (one per iteration). The original latch's IAdd
        // is now under an Unreachable terminator and will be pruned by
        // a later DCE pass; for this test we just count the clones.
        let total_iadds: usize = f
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .filter(|inst| matches!(inst.kind, InstKind::IAdd(..)))
            .count();
        assert!(
            total_iadds >= 4,
            "expected at least 4 IAdd insts (one acc-update per iteration), found {}",
            total_iadds
        );
    }

    #[test]
    fn does_not_unroll_load_bearing_loop() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("load_loop".into(), vec![], IrType::Void);

        let header_id = f.create_block("header");
        let latch_id = f.create_block("latch");
        let exit_id = f.create_block("exit");
        let entry_id = f.entry;

        let lo = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: lo,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let x_slot = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: x_slot,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I64)),
        });
        let y_slot = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: y_slot,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I64))),
            span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I64)),
        });
        let x_init = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: x_init,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(5, IntWidth::I64),
        });
        let store_x = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: store_x,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(x_init, x_slot),
        });
        let y_init = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: y_init,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let store_y = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: store_y,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(y_init, y_slot),
        });
        f.block_mut(entry_id).terminator = Some(Terminator::Branch(header_id, vec![lo]));

        let iv = f.next_value_id();
        f.block_mut(header_id).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I64),
        });
        let hi = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: hi,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(4, IntWidth::I64),
        });
        let cmp = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: cmp,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, hi),
        });
        f.block_mut(header_id).terminator = Some(Terminator::CondBranch {
            cond: cmp,
            true_dest: latch_id,
            true_args: vec![],
            false_dest: exit_id,
            false_args: vec![],
        });

        let x_val = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: x_val,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::Load(x_slot),
        });
        let y_val = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: y_val,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::Load(y_slot),
        });
        let sum = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: sum,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::IAdd(x_val, y_val),
        });
        let store_sum = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: store_sum,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(sum, y_slot),
        });
        let one = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: one,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let next = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: next,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::IAdd(iv, one),
        });
        f.block_mut(latch_id).terminator = Some(Terminator::Branch(header_id, vec![next]));

        f.block_mut(exit_id).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(
            !changed,
            "load-bearing loop should stay out of the full-unroll fast path"
        );
    }

    #[test]
    fn unrolls_trip1() {
        let mut m = build_counted_loop(3, 3); // lo == hi → 1 iteration
        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(changed, "trip-1 loop should unroll");
        let f = &m.functions[0];
        // entry + 1 cloned iteration block + exit = 3 blocks
        assert_eq!(f.blocks.len(), 3, "expected entry + 1 iter block + exit");
    }

    /// Regression: mem2reg places block params at dominance-frontier blocks,
    /// which can cause an outer-loop latch to reference the inner loop's header
    /// block param (%iv) in its branch terminator. After unrolling removes the
    /// inner header, %iv becomes undefined → IR verifier panic.
    ///
    /// `has_escaping_values` must check block params (not just instruction
    /// results) AND must be called for the header block (not just body_blocks).
    #[test]
    fn does_not_unroll_when_header_param_escapes_into_outer_block() {
        // Build: inner loop (entry → header(%iv) → latch → header, | → exit)
        //        plus an outer_latch block that passes %iv to outer_dest.
        // outer_latch is not part of the inner nl.body but references %iv.
        let mut m = Module::new("test".into());
        let mut f = Function::new("escape_test".into(), vec![], IrType::Void);

        let header_id = f.create_block("header");
        let latch_id = f.create_block("latch");
        let exit_id = f.create_block("exit");
        let outer_latch = f.create_block("outer_latch");
        let outer_dest = f.create_block("outer_dest");
        let entry_id = f.entry;

        // Entry: const 1; br header(1)
        let lo = f.next_value_id();
        f.block_mut(entry_id).insts.push(Inst {
            id: lo,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        f.block_mut(entry_id).terminator = Some(Terminator::Branch(header_id, vec![lo]));

        // Header: block param %iv; icmp %iv ≤ 3; condBr latch, exit
        let iv = f.next_value_id();
        f.block_mut(header_id).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I64),
        });
        let hi_c = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: hi_c,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(3, IntWidth::I64),
        });
        let cmp = f.next_value_id();
        f.block_mut(header_id).insts.push(Inst {
            id: cmp,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, hi_c),
        });
        f.block_mut(header_id).terminator = Some(Terminator::CondBranch {
            cond: cmp,
            true_dest: latch_id,
            true_args: vec![],
            false_dest: exit_id,
            false_args: vec![],
        });

        // Latch: %next = iadd %iv, 1; br header(%next)
        let one = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: one,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let nxt = f.next_value_id();
        f.block_mut(latch_id).insts.push(Inst {
            id: nxt,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::IAdd(iv, one),
        });
        f.block_mut(latch_id).terminator = Some(Terminator::Branch(header_id, vec![nxt]));

        // Exit: ret void
        f.block_mut(exit_id).terminator = Some(Terminator::Return(None));

        // outer_latch: br outer_dest(%iv)
        // Simulates an outer-loop latch that mem2reg threaded %iv through.
        // %iv is the header's block param — it escapes into this external block.
        f.block_mut(outer_latch).terminator = Some(Terminator::Branch(outer_dest, vec![iv]));

        // outer_dest(%x): ret void
        let x = f.next_value_id();
        f.block_mut(outer_dest).params.push(BlockParam {
            id: x,
            ty: IrType::Int(IntWidth::I64),
        });
        f.block_mut(outer_dest).terminator = Some(Terminator::Return(None));

        m.add_function(f);

        let pass = LoopUnroll;
        let changed = pass.run(&mut m);
        assert!(
            !changed,
            "must not unroll when header block param escapes into an outer-loop block"
        );
    }
}
