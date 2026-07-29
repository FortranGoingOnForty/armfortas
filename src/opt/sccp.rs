//! Sparse Conditional Constant Propagation (SCCP) pass.
//!
//! Combines constant tracking with reachability analysis. The key
//! insight over plain `const_fold` + `const_prop` is that constants
//! flowing into a block parameter via a CFG edge whose source is
//! statically unreachable do **not** force the parameter to Bottom.
//! That lets SCCP fold cases the basic passes can't reach, e.g.
//! ```text
//! entry: br .true., a, b
//! a:     br merge(c1)
//! b:     br merge(c2)             ; b unreachable from entry
//! merge: param p; ... use p ...
//! ```
//! After SCCP, the merge param `p` is constant `c1` because only the
//! `a → merge` edge is reachable.
//!
//! Algorithm (Wegman-Zadeck, 1991):
//!  1. Lattice: `Top` (not yet seen), `Const(c)` (proven constant),
//!     `Bottom` (overdefined / non-constant).
//!  2. Reachability: a `HashSet<BlockId>` of reachable blocks plus a
//!     `HashSet<EdgeId>` of reachable CFG edges. Edge identity includes
//!     the terminator arm, so parallel edges remain distinct.
//!  3. Two worklists:
//!      * SSA-edge worklist — re-evaluate uses when a value's lattice
//!        moves down (Top → Const, Top → Bottom, Const → Bottom).
//!      * CFG-edge worklist — when a new edge becomes reachable,
//!        re-meet the destination block's params.
//!  4. Iterate until both worklists are empty.
//!
//! After fixpoint we materialize the result:
//!  * Each constant block-param is rewritten: a `Const*` instruction
//!    is inserted at the start of its block; uses are redirected;
//!    the param entry and matching predecessor args are dropped.
//!  * Each constant-condition branch is folded to an unconditional
//!    `Branch` to the live target.
//!  * `prune_unreachable` reaps the now-dead blocks.
//!
//! Per-instruction arithmetic transfer (e.g. `IAdd Const Const →
//! Const`) is intentionally left to `const_fold`. The pass-manager
//! fixpoint composes them: SCCP exposes new reachability →
//! `const_fold` propagates → SCCP runs again. This keeps SCCP narrow
//! and avoids duplicating the dozens of width-aware folds in
//! `const_fold::try_fold`.

use super::pass::Pass;
use super::util::prune_unreachable;
use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IntWidth, IrType};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq)]
enum ConstVal {
    Int(i128, IntWidth),
    Float(u64, FloatWidth), // bit pattern — avoids NaN equality issues
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Lattice {
    Top,
    Const(ConstVal),
    Bottom,
}

impl ConstVal {
    fn from_inst(kind: &InstKind) -> Option<Self> {
        match kind {
            InstKind::ConstInt(v, w) => Some(ConstVal::Int(sext(*v, w.bits()), *w)),
            InstKind::ConstFloat(v, w) => Some(ConstVal::Float(v.to_bits(), *w)),
            InstKind::ConstBool(b) => Some(ConstVal::Bool(*b)),
            _ => None,
        }
    }

    fn to_inst_kind(self) -> InstKind {
        match self {
            ConstVal::Int(v, w) => InstKind::ConstInt(v, w),
            ConstVal::Float(bits, w) => InstKind::ConstFloat(f64::from_bits(bits), w),
            ConstVal::Bool(b) => InstKind::ConstBool(b),
        }
    }

    fn ir_type(self) -> IrType {
        match self {
            ConstVal::Int(_, w) => IrType::Int(w),
            ConstVal::Float(_, w) => IrType::Float(w),
            ConstVal::Bool(_) => IrType::Bool,
        }
    }
}

fn sext(v: i128, bits: u32) -> i128 {
    if bits >= 128 {
        v
    } else {
        let shift = 128 - bits;
        (v << shift) >> shift
    }
}

fn meet(a: Lattice, b: Lattice) -> Lattice {
    match (a, b) {
        (Lattice::Top, x) | (x, Lattice::Top) => x,
        (Lattice::Bottom, _) | (_, Lattice::Bottom) => Lattice::Bottom,
        (Lattice::Const(x), Lattice::Const(y)) => {
            if x == y {
                Lattice::Const(x)
            } else {
                Lattice::Bottom
            }
        }
    }
}

struct Sccp<'a> {
    func: &'a Function,
    /// Reverse map: ValueId → its defining block id (for SSA-edge
    /// worklist processing of block params, since we need to know
    /// which block's incoming edges to re-meet).
    block_param_owner: HashMap<ValueId, BlockId>,
    /// Lattice value per ValueId. Absent = Top.
    lattice: HashMap<ValueId, Lattice>,
    /// Reachable blocks.
    reachable_blocks: HashSet<BlockId>,
    /// Reachable CFG edges, identified by predecessor and terminator arm.
    reachable_edges: HashSet<EdgeId>,
    /// CFG worklist — newly reachable edges to process.
    cfg_worklist: VecDeque<EdgeId>,
    /// SSA worklist — values whose lattice changed; re-evaluate users
    /// (block params and terminators that consume them).
    ssa_worklist: VecDeque<ValueId>,
    /// Incoming edge identities for each destination block.
    incoming_edges: HashMap<BlockId, Vec<EdgeId>>,
    /// Destination and block arguments for each distinct CFG edge.
    edge_data: HashMap<EdgeId, EdgeData>,
    /// For each block param ValueId: the (pred → succ) edges that
    /// feed it, plus its index within the block's param list. We
    /// rebuild the meet over reachable edges whenever any of the
    /// inputs changes.
    param_inputs: HashMap<ValueId, ParamInputInfo>,
    /// For each ValueId, the set of dependents we should requeue on
    /// SSA-worklist when it changes. For now: only block params and
    /// terminator-conditions are tracked (we don't model arithmetic).
    value_users: HashMap<ValueId, Vec<ValueUser>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EdgeId {
    from: BlockId,
    arm: EdgeArm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EdgeArm {
    Branch,
    CondTrue,
    CondFalse,
    SwitchDefault,
    SwitchCase(usize),
}

#[derive(Debug, Clone)]
struct EdgeData {
    to: BlockId,
    args: Vec<ValueId>,
}

#[derive(Debug, Clone)]
struct ParamInputInfo {
    block: BlockId,
    param_idx: usize,
}

#[derive(Debug, Clone)]
enum ValueUser {
    /// `cond` of a CondBranch in the given block; reanalyze the
    /// terminator to maybe expose newly-unreachable successors.
    Terminator(BlockId),
    /// A block param that consumes this value along some edge —
    /// re-meet the param when the value's lattice changes.
    BlockParam(ValueId),
}

impl<'a> Sccp<'a> {
    fn new(func: &'a Function) -> Self {
        let mut block_param_owner = HashMap::new();
        let mut param_inputs = HashMap::new();
        for block in &func.blocks {
            for (i, p) in block.params.iter().enumerate() {
                block_param_owner.insert(p.id, block.id);
                param_inputs.insert(
                    p.id,
                    ParamInputInfo {
                        block: block.id,
                        param_idx: i,
                    },
                );
            }
        }

        let mut incoming_edges: HashMap<BlockId, Vec<EdgeId>> = HashMap::new();
        let mut edge_data: HashMap<EdgeId, EdgeData> = HashMap::new();
        for block in &func.blocks {
            incoming_edges.entry(block.id).or_default();
        }
        {
            let mut record_edge = |edge: EdgeId, to: BlockId, args: Vec<ValueId>| {
                incoming_edges.entry(to).or_default().push(edge);
                edge_data.insert(edge, EdgeData { to, args });
            };
            for block in &func.blocks {
                let Some(term) = &block.terminator else {
                    continue;
                };
                match term {
                    Terminator::Branch(succ, args) => {
                        record_edge(
                            EdgeId {
                                from: block.id,
                                arm: EdgeArm::Branch,
                            },
                            *succ,
                            args.clone(),
                        );
                    }
                    Terminator::CondBranch {
                        true_dest,
                        true_args,
                        false_dest,
                        false_args,
                        ..
                    } => {
                        record_edge(
                            EdgeId {
                                from: block.id,
                                arm: EdgeArm::CondTrue,
                            },
                            *true_dest,
                            true_args.clone(),
                        );
                        record_edge(
                            EdgeId {
                                from: block.id,
                                arm: EdgeArm::CondFalse,
                            },
                            *false_dest,
                            false_args.clone(),
                        );
                    }
                    Terminator::Switch { cases, default, .. } => {
                        record_edge(
                            EdgeId {
                                from: block.id,
                                arm: EdgeArm::SwitchDefault,
                            },
                            *default,
                            vec![],
                        );
                        for (case_index, (_, target)) in cases.iter().enumerate() {
                            record_edge(
                                EdgeId {
                                    from: block.id,
                                    arm: EdgeArm::SwitchCase(case_index),
                                },
                                *target,
                                vec![],
                            );
                        }
                    }
                    Terminator::Return(_) | Terminator::Unreachable => {}
                }
            }
        }

        // Build dependency graph: for each value, the users that need
        // to be reanalyzed when it changes.
        let mut value_users: HashMap<ValueId, Vec<ValueUser>> = HashMap::new();
        for block in &func.blocks {
            // Block-param dependents: each param's incoming arg
            // along each pred edge.
            for (pi, p) in block.params.iter().enumerate() {
                for edge in incoming_edges.get(&block.id).cloned().unwrap_or_default() {
                    if let Some(data) = edge_data.get(&edge) {
                        if let Some(arg) = data.args.get(pi) {
                            value_users
                                .entry(*arg)
                                .or_default()
                                .push(ValueUser::BlockParam(p.id));
                        }
                    }
                }
            }
            // Terminator-condition dependents (only CondBranch/Switch
            // can fold on a constant condition).
            if let Some(term) = &block.terminator {
                match term {
                    Terminator::CondBranch { cond, .. } => {
                        value_users
                            .entry(*cond)
                            .or_default()
                            .push(ValueUser::Terminator(block.id));
                    }
                    Terminator::Switch { selector, .. } => {
                        value_users
                            .entry(*selector)
                            .or_default()
                            .push(ValueUser::Terminator(block.id));
                    }
                    _ => {}
                }
            }
        }

        Self {
            func,
            block_param_owner,
            lattice: HashMap::new(),
            reachable_blocks: HashSet::new(),
            reachable_edges: HashSet::new(),
            cfg_worklist: VecDeque::new(),
            ssa_worklist: VecDeque::new(),
            incoming_edges,
            edge_data,
            param_inputs,
            value_users,
        }
    }

    fn lat(&self, v: ValueId) -> Lattice {
        self.lattice.get(&v).copied().unwrap_or(Lattice::Top)
    }

    /// Set lattice value, queue SSA-worklist if changed and not
    /// already at the bottom.
    fn set_lat(&mut self, v: ValueId, new: Lattice) {
        let old = self.lat(v);
        let merged = meet(old, new);
        if merged != old {
            self.lattice.insert(v, merged);
            self.ssa_worklist.push_back(v);
        }
    }

    /// Make a block reachable (idempotent). Seeds CFG worklist with
    /// outgoing edges and marks the block.
    fn mark_block_reachable(&mut self, b: BlockId) {
        if !self.reachable_blocks.insert(b) {
            return;
        }
        // First time reaching this block — also seed its outgoing
        // edges based on its (current) terminator analysis.
        self.process_terminator(b);
    }

    /// Mark a CFG edge reachable; if newly so, mark the destination
    /// block reachable and queue the edge for block-param re-meet.
    fn mark_edge(&mut self, edge: EdgeId) {
        let Some(to) = self.edge_data.get(&edge).map(|data| data.to) else {
            return;
        };
        if self.reachable_edges.insert(edge) {
            self.cfg_worklist.push_back(edge);
            self.mark_block_reachable(to);
        }
    }

    /// Analyze a block's terminator and mark live successors.
    /// Called when the block becomes reachable or when its
    /// terminator-condition lattice changes.
    fn process_terminator(&mut self, b: BlockId) {
        let Some(block) = self.func.try_block(b) else {
            return;
        };
        let Some(term) = &block.terminator else {
            return;
        };
        match term {
            Terminator::Return(_) | Terminator::Unreachable => {}
            Terminator::Branch(_, _) => {
                self.mark_edge(EdgeId {
                    from: b,
                    arm: EdgeArm::Branch,
                });
            }
            Terminator::CondBranch { cond, .. } => match self.lat(*cond) {
                Lattice::Const(ConstVal::Bool(true)) => self.mark_edge(EdgeId {
                    from: b,
                    arm: EdgeArm::CondTrue,
                }),
                Lattice::Const(ConstVal::Bool(false)) => self.mark_edge(EdgeId {
                    from: b,
                    arm: EdgeArm::CondFalse,
                }),
                Lattice::Bottom => {
                    self.mark_edge(EdgeId {
                        from: b,
                        arm: EdgeArm::CondTrue,
                    });
                    self.mark_edge(EdgeId {
                        from: b,
                        arm: EdgeArm::CondFalse,
                    });
                }
                // Const(non-bool) is a type error in well-typed IR;
                // be conservative and mark both. Top means we'll
                // revisit when `cond` resolves.
                Lattice::Const(_) => {
                    self.mark_edge(EdgeId {
                        from: b,
                        arm: EdgeArm::CondTrue,
                    });
                    self.mark_edge(EdgeId {
                        from: b,
                        arm: EdgeArm::CondFalse,
                    });
                }
                Lattice::Top => {}
            },
            Terminator::Switch {
                selector, cases, ..
            } => match self.lat(*selector) {
                Lattice::Const(ConstVal::Int(sv, w)) => {
                    let bits = w.bits();
                    let arm = cases
                        .iter()
                        .position(|(k, _)| sext(*k as i128, bits) == sv)
                        .map(EdgeArm::SwitchCase)
                        .unwrap_or(EdgeArm::SwitchDefault);
                    self.mark_edge(EdgeId { from: b, arm });
                }
                Lattice::Bottom => {
                    for case_index in 0..cases.len() {
                        self.mark_edge(EdgeId {
                            from: b,
                            arm: EdgeArm::SwitchCase(case_index),
                        });
                    }
                    self.mark_edge(EdgeId {
                        from: b,
                        arm: EdgeArm::SwitchDefault,
                    });
                }
                Lattice::Const(_) => {
                    for case_index in 0..cases.len() {
                        self.mark_edge(EdgeId {
                            from: b,
                            arm: EdgeArm::SwitchCase(case_index),
                        });
                    }
                    self.mark_edge(EdgeId {
                        from: b,
                        arm: EdgeArm::SwitchDefault,
                    });
                }
                Lattice::Top => {}
            },
        }
    }

    /// Re-meet a block param's lattice value over all reachable
    /// incoming edges.
    fn recompute_param(&mut self, param_id: ValueId) {
        let Some(info) = self.param_inputs.get(&param_id).cloned() else {
            return;
        };
        let block = info.block;
        let pi = info.param_idx;
        let incoming_edges = self.incoming_edges.get(&block).cloned().unwrap_or_default();
        let mut new = Lattice::Top;
        for edge in incoming_edges {
            if !self.reachable_edges.contains(&edge) {
                continue;
            }
            let Some(data) = self.edge_data.get(&edge) else {
                continue;
            };
            let Some(arg) = data.args.get(pi) else {
                continue;
            };
            // If the arg IS the param itself (back-edge with same
            // value), don't pollute with our own current state.
            // SSA permits this, e.g. loop induction merges.
            if *arg == param_id {
                continue;
            }
            new = meet(new, self.lat(*arg));
            if new == Lattice::Bottom {
                break;
            }
        }
        // Convert "Top with at least one reachable edge" → keep Top
        // until a concrete arg shows up (Top is sound: still optimistic).
        // monotone update via set_lat.
        let old = self.lat(param_id);
        let merged = meet(old, new);
        if merged != old {
            self.lattice.insert(param_id, merged);
            self.ssa_worklist.push_back(param_id);
        }
    }

    fn run(&mut self) {
        // Seed lattice for every constant-defining instruction.
        for block in &self.func.blocks {
            for inst in &block.insts {
                if let Some(c) = ConstVal::from_inst(&inst.kind) {
                    self.lattice.insert(inst.id, Lattice::Const(c));
                } else {
                    // Non-const, non-block-param values default to
                    // Bottom — SCCP doesn't model arithmetic
                    // transfer. const_fold handles those after we
                    // expose new reachability.
                    self.lattice.insert(inst.id, Lattice::Bottom);
                }
            }
        }
        // Function params are unknown → Bottom.
        for p in &self.func.params {
            self.lattice.insert(p.id, Lattice::Bottom);
        }

        // Entry is always reachable.
        self.mark_block_reachable(self.func.entry);

        while !self.cfg_worklist.is_empty() || !self.ssa_worklist.is_empty() {
            while let Some(edge) = self.cfg_worklist.pop_front() {
                let Some(to) = self.edge_data.get(&edge).map(|data| data.to) else {
                    continue;
                };
                // Re-meet every block param of `to`, since a new
                // reachable edge may add a new input.
                let params: Vec<ValueId> = self
                    .func
                    .try_block(to)
                    .map(|b| b.params.iter().map(|p| p.id).collect())
                    .unwrap_or_default();
                for pid in params {
                    self.recompute_param(pid);
                }
            }
            while let Some(v) = self.ssa_worklist.pop_front() {
                let users = self.value_users.get(&v).cloned().unwrap_or_default();
                for u in users {
                    match u {
                        ValueUser::Terminator(b) => {
                            if self.reachable_blocks.contains(&b) {
                                self.process_terminator(b);
                            }
                        }
                        ValueUser::BlockParam(pid) => {
                            self.recompute_param(pid);
                        }
                    }
                }
                // Block params themselves change → reanalyze
                // terminators that consume them. Already handled by
                // `value_users`.
                let _ = self.block_param_owner.get(&v);
            }
        }
    }
}

/// Result of SCCP analysis — the data needed by `apply` without
/// holding any borrow on the function.
struct SccpResult {
    lattice: HashMap<ValueId, Lattice>,
}

/// Apply the SCCP analysis result to `func`. Returns whether anything
/// was rewritten.
fn apply(func: &mut Function, analysis: &SccpResult) -> bool {
    let mut changed = false;

    // Step 1: rewrite constant block params.
    //
    // For every block param whose lattice resolved to `Const`:
    //   - Insert a fresh `Const*` instruction at the front of its
    //     block, allocate a new ValueId for it.
    //   - Substitute uses of the block-param ValueId with the new
    //     constant ValueId across the function.
    //   - Remove the param from the block's `params`.
    //   - Remove the matching argument from every predecessor's
    //     branch terminator.
    //
    // We do this block-by-block, scanning back→front within the
    // params list so index-based predecessor arg removal stays
    // valid.
    let mut const_param_rewrites: Vec<(BlockId, usize, ValueId, ConstVal, IrType)> = Vec::new();
    for block in &func.blocks {
        for (pi, p) in block.params.iter().enumerate() {
            if let Some(Lattice::Const(c)) = analysis.lattice.get(&p.id).copied() {
                // Sanity: lattice type should be compatible with
                // declared param type. If not (defensive), skip.
                if !ty_compatible(&p.ty, &c.ir_type()) {
                    continue;
                }
                const_param_rewrites.push((block.id, pi, p.id, c, p.ty.clone()));
            }
        }
    }

    if !const_param_rewrites.is_empty() {
        // Sort by (block, descending param index) so predecessor
        // arg removal is index-stable as we mutate.
        const_param_rewrites.sort_by_key(|x| std::cmp::Reverse(x.1));
        for (block_id, pi, param_id, cval, ty) in const_param_rewrites {
            // Allocate a new value for the constant.
            let new_id = func.next_value_id();
            // Insert at the front of the target block.
            if let Some(block) = func.try_block_mut(block_id) {
                let span = block
                    .insts
                    .first()
                    .map(|i| i.span)
                    .or_else(|| {
                        block.terminator.as_ref().map(|_| crate::lexer::Span {
                            start: crate::lexer::Position { line: 1, col: 1 },
                            end: crate::lexer::Position { line: 1, col: 1 },
                            file_id: 0,
                        })
                    })
                    .unwrap_or(crate::lexer::Span {
                        start: crate::lexer::Position { line: 1, col: 1 },
                        end: crate::lexer::Position { line: 1, col: 1 },
                        file_id: 0,
                    });
                block.insts.insert(
                    0,
                    Inst {
                        id: new_id,
                        kind: cval.to_inst_kind(),
                        ty: ty.clone(),
                        span,
                    },
                );
                // Drop the param.
                block.params.remove(pi);
            }
            // Rewrite uses of param_id → new_id.
            crate::ir::walk::substitute_uses(func, param_id, new_id);
            // Drop the matching argument in every predecessor's
            // branch.
            for pred_block in &mut func.blocks {
                let Some(term) = pred_block.terminator.as_mut() else {
                    continue;
                };
                match term {
                    Terminator::Branch(dest, args) if *dest == block_id && pi < args.len() => {
                        args.remove(pi);
                    }
                    Terminator::CondBranch {
                        true_dest,
                        true_args,
                        false_dest,
                        false_args,
                        ..
                    } => {
                        if *true_dest == block_id && pi < true_args.len() {
                            true_args.remove(pi);
                        }
                        if *false_dest == block_id && pi < false_args.len() {
                            false_args.remove(pi);
                        }
                    }
                    _ => {}
                }
            }
            changed = true;
        }
        func.rebuild_type_cache();
    }

    // Step 2: fold constant-condition terminators.
    //
    // Re-collect const map (block-param rewrites just added new Const
    // instructions).
    let mut consts: HashMap<ValueId, ConstVal> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(c) = ConstVal::from_inst(&inst.kind) {
                consts.insert(inst.id, c);
            }
        }
    }
    let mut folded_any = false;
    for block in &mut func.blocks {
        let Some(term) = block.terminator.take() else {
            continue;
        };
        let new_term = match term {
            Terminator::CondBranch {
                cond,
                true_dest,
                true_args,
                false_dest,
                false_args,
            } => match consts.get(&cond) {
                Some(ConstVal::Bool(true)) => {
                    folded_any = true;
                    Terminator::Branch(true_dest, true_args)
                }
                Some(ConstVal::Bool(false)) => {
                    folded_any = true;
                    Terminator::Branch(false_dest, false_args)
                }
                _ => Terminator::CondBranch {
                    cond,
                    true_dest,
                    true_args,
                    false_dest,
                    false_args,
                },
            },
            Terminator::Switch {
                selector,
                cases,
                default,
            } => match consts.get(&selector) {
                Some(ConstVal::Int(sv, w)) => {
                    folded_any = true;
                    let bits = w.bits();
                    let target = cases
                        .iter()
                        .find(|(k, _)| sext(*k as i128, bits) == *sv)
                        .map(|(_, blk)| *blk)
                        .unwrap_or(default);
                    Terminator::Branch(target, vec![])
                }
                _ => Terminator::Switch {
                    selector,
                    cases,
                    default,
                },
            },
            other => other,
        };
        block.terminator = Some(new_term);
    }
    if folded_any {
        changed = true;
    }

    // Step 3: prune unreachable blocks.
    //
    // The SCCP analysis itself produced a reachability set, but the
    // safe thing is to recompute it post-rewrite — block-param
    // rewrites and terminator folds can change the CFG.
    if prune_unreachable(func) {
        changed = true;
    }
    if changed {
        func.rebuild_type_cache();
    }

    changed
}

fn ty_compatible(a: &IrType, b: &IrType) -> bool {
    // Cheap structural compare for the const types we materialize.
    match (a, b) {
        (IrType::Bool, IrType::Bool) => true,
        (IrType::Int(wa), IrType::Int(wb)) => wa == wb,
        (IrType::Float(wa), IrType::Float(wb)) => wa == wb,
        _ => false,
    }
}

/// The pass entry point.
pub struct Sccp_;

impl Pass for Sccp_ {
    fn name(&self) -> &'static str {
        "sccp"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            let analysis = {
                let mut s = Sccp::new(func);
                s.run();
                SccpResult { lattice: s.lattice }
            };
            if apply(func, &analysis) {
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::ir::verify::verify_module;
    use crate::lexer::{Position, Span};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span {
            start: p,
            end: p,
            file_id: 0,
        }
    }

    #[test]
    fn folds_constant_true_condbranch() {
        // entry: cond = const(true); cond_branch cond, then, else
        // then:  ret
        // else:  ret
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let bb_t = f.create_block("then");
        let bb_e = f.create_block("else");
        let cond_id = f.next_value_id();
        f.block_mut(f.entry).insts.push(Inst {
            id: cond_id,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: cond_id,
            true_dest: bb_t,
            true_args: vec![],
            false_dest: bb_e,
            false_args: vec![],
        });
        f.block_mut(bb_t).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_e).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(Sccp_.run(&mut m));
        let f = &m.functions[0];
        match &f.blocks[0].terminator {
            Some(Terminator::Branch(d, _)) => assert_eq!(*d, bb_t),
            other => panic!("expected Branch, got {:?}", other),
        }
        // else block should be pruned.
        assert!(f.blocks.iter().all(|b| b.id != bb_e));
    }

    #[test]
    fn merge_param_with_uniform_const_resolves() {
        // entry:  cond=const(true); cond_branch cond, a, b
        // a:      branch merge(const 7)
        // b:      branch merge(const 99)   ; statically unreachable
        // merge:  param p; ret p
        //
        // Plain const_prop can't fold this — `p` has two distinct
        // incoming arg values, and const_prop doesn't know `b` is
        // unreachable. SCCP does.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

        let bb_a = f.create_block("a");
        let bb_b = f.create_block("b");
        let bb_merge = f.create_block("merge");

        let cond_id = f.next_value_id();
        let const_a = f.next_value_id();
        let const_b = f.next_value_id();
        let merge_param = f.next_value_id();

        f.block_mut(bb_merge).params.push(BlockParam {
            id: merge_param,
            ty: IrType::Int(IntWidth::I32),
        });

        f.block_mut(f.entry).insts.push(Inst {
            id: cond_id,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: cond_id,
            true_dest: bb_a,
            true_args: vec![],
            false_dest: bb_b,
            false_args: vec![],
        });

        f.block_mut(bb_a).insts.push(Inst {
            id: const_a,
            kind: InstKind::ConstInt(7, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(bb_a).terminator = Some(Terminator::Branch(bb_merge, vec![const_a]));

        f.block_mut(bb_b).insts.push(Inst {
            id: const_b,
            kind: InstKind::ConstInt(99, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(bb_b).terminator = Some(Terminator::Branch(bb_merge, vec![const_b]));

        f.block_mut(bb_merge).terminator = Some(Terminator::Return(Some(merge_param)));
        f.rebuild_type_cache();
        m.add_function(f);

        assert!(Sccp_.run(&mut m), "SCCP should fold this");

        let f = &m.functions[0];
        // merge block: param removed, new ConstInt(7) inserted, ret
        // now references that const.
        let merge = f.blocks.iter().find(|b| b.id == bb_merge).unwrap();
        assert!(merge.params.is_empty(), "merge param should be gone");
        // First inst should be a ConstInt(7).
        match merge.insts.first().map(|i| &i.kind) {
            Some(InstKind::ConstInt(7, IntWidth::I32)) => {}
            other => panic!(
                "expected ConstInt(7,I32) at start of merge, got {:?}",
                other
            ),
        }
        // bb_b is unreachable post-fold and should be pruned.
        assert!(
            !f.blocks.iter().any(|b| b.id == bb_b),
            "unreachable b should be pruned"
        );
    }

    #[test]
    fn non_constant_param_is_not_rewritten() {
        // entry: cond_branch <param> a, b
        // a:     branch merge(const 1)
        // b:     branch merge(const 2)
        // merge: ret param
        //
        // Both arms reachable, two distinct constants → param stays.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let params = vec![Param {
            name: "c".into(),
            ty: IrType::Bool,
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut f = Function::new("f".into(), params, IrType::Int(IntWidth::I32));
        let bb_a = f.create_block("a");
        let bb_b = f.create_block("b");
        let bb_merge = f.create_block("merge");

        let const_a = f.next_value_id();
        let const_b = f.next_value_id();
        let merge_param = f.next_value_id();
        f.block_mut(bb_merge).params.push(BlockParam {
            id: merge_param,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: ValueId(0),
            true_dest: bb_a,
            true_args: vec![],
            false_dest: bb_b,
            false_args: vec![],
        });
        f.block_mut(bb_a).insts.push(Inst {
            id: const_a,
            kind: InstKind::ConstInt(1, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(bb_a).terminator = Some(Terminator::Branch(bb_merge, vec![const_a]));
        f.block_mut(bb_b).insts.push(Inst {
            id: const_b,
            kind: InstKind::ConstInt(2, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(bb_b).terminator = Some(Terminator::Branch(bb_merge, vec![const_b]));
        f.block_mut(bb_merge).terminator = Some(Terminator::Return(Some(merge_param)));
        f.rebuild_type_cache();
        m.add_function(f);

        let _changed = Sccp_.run(&mut m);
        let f = &m.functions[0];
        // The merge block's param must remain — both arms are
        // genuinely reachable and disagree.
        let merge = f.blocks.iter().find(|b| b.id == bb_merge).unwrap();
        assert_eq!(
            merge.params.len(),
            1,
            "merge param should survive when both arms are reachable and disagree"
        );
    }

    #[test]
    fn unknown_cond_left_alone() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let params = vec![Param {
            name: "p".into(),
            ty: IrType::Bool,
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut f = Function::new("f".into(), params, IrType::Void);
        let bb_t = f.create_block("then");
        let bb_e = f.create_block("else");
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: ValueId(0),
            true_dest: bb_t,
            true_args: vec![],
            false_dest: bb_e,
            false_args: vec![],
        });
        f.block_mut(bb_t).terminator = Some(Terminator::Return(None));
        f.block_mut(bb_e).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Sccp_.run(&mut m));
        assert!(matches!(
            m.functions[0].blocks[0].terminator,
            Some(Terminator::CondBranch { .. })
        ));
    }

    #[test]
    fn same_target_condbranch_uses_the_selected_edge_argument() {
        // Both arms target `merge`, but they are distinct CFG edges with
        // distinct block arguments. A true condition must select 7, not the
        // false edge's 99.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let merge = f.create_block("merge");
        let cond = f.next_value_id();
        let true_value = f.next_value_id();
        let false_value = f.next_value_id();
        let merge_param = f.next_value_id();

        f.block_mut(f.entry).insts.extend([
            Inst {
                id: cond,
                kind: InstKind::ConstBool(true),
                ty: IrType::Bool,
                span: dummy_span(),
            },
            Inst {
                id: true_value,
                kind: InstKind::ConstInt(7, IntWidth::I32),
                ty: IrType::Int(IntWidth::I32),
                span: dummy_span(),
            },
            Inst {
                id: false_value,
                kind: InstKind::ConstInt(99, IntWidth::I32),
                ty: IrType::Int(IntWidth::I32),
                span: dummy_span(),
            },
        ]);
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: merge,
            true_args: vec![true_value],
            false_dest: merge,
            false_args: vec![false_value],
        });
        f.block_mut(merge).params.push(BlockParam {
            id: merge_param,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(merge).terminator = Some(Terminator::Return(Some(merge_param)));
        f.rebuild_type_cache();
        m.add_function(f);

        assert!(
            verify_module(&m).is_empty(),
            "same-target test IR must start valid"
        );
        assert!(Sccp_.run(&mut m), "SCCP should fold the known edge");
        assert!(
            verify_module(&m).is_empty(),
            "SCCP must preserve valid same-target IR"
        );
        let merge = m.functions[0]
            .blocks
            .iter()
            .find(|block| block.id == merge)
            .expect("merge block");
        assert!(merge.params.is_empty(), "known merge value should fold");
        assert!(
            matches!(
                merge.insts.first().map(|inst| &inst.kind),
                Some(InstKind::ConstInt(7, IntWidth::I32))
            ),
            "SCCP selected the wrong parallel-edge argument: {:?}",
            merge.insts
        );
    }

    #[test]
    fn same_target_condbranch_with_unknown_condition_keeps_both_edges() {
        // With an unknown condition both parallel edges are executable. Their
        // different arguments must meet to Bottom, leaving the block parameter
        // intact rather than replacing it with either arm's value.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let params = vec![Param {
            name: "condition".into(),
            ty: IrType::Bool,
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut f = Function::new("f".into(), params, IrType::Int(IntWidth::I32));
        let merge = f.create_block("merge");
        let true_value = f.next_value_id();
        let false_value = f.next_value_id();
        let merge_param = f.next_value_id();

        f.block_mut(f.entry).insts.extend([
            Inst {
                id: true_value,
                kind: InstKind::ConstInt(7, IntWidth::I32),
                ty: IrType::Int(IntWidth::I32),
                span: dummy_span(),
            },
            Inst {
                id: false_value,
                kind: InstKind::ConstInt(99, IntWidth::I32),
                ty: IrType::Int(IntWidth::I32),
                span: dummy_span(),
            },
        ]);
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond: ValueId(0),
            true_dest: merge,
            true_args: vec![true_value],
            false_dest: merge,
            false_args: vec![false_value],
        });
        f.block_mut(merge).params.push(BlockParam {
            id: merge_param,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(merge).terminator = Some(Terminator::Return(Some(merge_param)));
        f.rebuild_type_cache();
        m.add_function(f);

        assert!(
            verify_module(&m).is_empty(),
            "same-target test IR must start valid"
        );
        let _ = Sccp_.run(&mut m);
        assert!(
            verify_module(&m).is_empty(),
            "SCCP must preserve valid same-target IR"
        );
        let merge = m.functions[0]
            .blocks
            .iter()
            .find(|block| block.id == merge)
            .expect("merge block");
        assert_eq!(
            merge.params.len(),
            1,
            "distinct executable edges must keep the merge parameter"
        );
    }
}
