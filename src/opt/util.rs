//! Shared helpers used by multiple optimization passes.
//!
//! This module exists so the operand-walking machinery only lives in
//! one place. Each pass that needs to enumerate operands, substitute
//! uses, or compute reachability imports from here instead of
//! reimplementing — that way a future change to the IR (a new
//! `InstKind` variant, say) only requires updating one match.

use crate::ir::inst::*;
use std::collections::{HashSet, VecDeque};

/// All `ValueId`s consumed as operands by an instruction.
pub fn inst_uses(kind: &InstKind) -> Vec<ValueId> {
    match kind {
        InstKind::ConstInt(..) | InstKind::ConstFloat(..) |
        InstKind::ConstBool(..) | InstKind::ConstString(..) |
        InstKind::Undef(..) | InstKind::Alloca(..) => vec![],

        InstKind::IAdd(a, b) | InstKind::ISub(a, b) |
        InstKind::IMul(a, b) | InstKind::IDiv(a, b) |
        InstKind::IMod(a, b) => vec![*a, *b],
        InstKind::INeg(a) => vec![*a],

        InstKind::FAdd(a, b) | InstKind::FSub(a, b) |
        InstKind::FMul(a, b) | InstKind::FDiv(a, b) |
        InstKind::FPow(a, b) => vec![*a, *b],
        InstKind::FNeg(a) | InstKind::FAbs(a) | InstKind::FSqrt(a) => vec![*a],

        InstKind::ICmp(_, a, b) | InstKind::FCmp(_, a, b) => vec![*a, *b],

        InstKind::And(a, b) | InstKind::Or(a, b) => vec![*a, *b],
        InstKind::Not(a) => vec![*a],

        InstKind::Select(c, t, f) => vec![*c, *t, *f],

        InstKind::BitAnd(a, b) | InstKind::BitOr(a, b) |
        InstKind::BitXor(a, b) | InstKind::Shl(a, b) |
        InstKind::LShr(a, b) | InstKind::AShr(a, b) => vec![*a, *b],
        InstKind::BitNot(a) | InstKind::CountLeadingZeros(a) |
        InstKind::CountTrailingZeros(a) | InstKind::PopCount(a) => vec![*a],

        InstKind::IntToFloat(v, _) | InstKind::FloatToInt(v, _) |
        InstKind::FloatExtend(v, _) | InstKind::FloatTrunc(v, _) |
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) => vec![*v],

        InstKind::Load(a) => vec![*a],
        InstKind::Store(v, a) => vec![*v, *a],
        InstKind::GetElementPtr(base, idxs) => {
            let mut uses = vec![*base];
            uses.extend(idxs);
            uses
        }

        InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => args.clone(),

        InstKind::ExtractField(agg, _) => vec![*agg],
        InstKind::InsertField(agg, _, val) => vec![*agg, *val],
    }
}

/// All `ValueId`s consumed by a terminator.
pub fn terminator_uses(term: &Terminator) -> Vec<ValueId> {
    match term {
        Terminator::Return(None) | Terminator::Unreachable => vec![],
        Terminator::Return(Some(v)) => vec![*v],
        Terminator::Branch(_, args) => args.clone(),
        Terminator::CondBranch { cond, true_args, false_args, .. } => {
            let mut uses = vec![*cond];
            uses.extend(true_args);
            uses.extend(false_args);
            uses
        }
        Terminator::Switch { selector, .. } => vec![*selector],
    }
}

/// All successor `BlockId`s of a terminator.
pub fn terminator_targets(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Return(_) | Terminator::Unreachable => vec![],
        Terminator::Branch(d, _) => vec![*d],
        Terminator::CondBranch { true_dest, false_dest, .. } => vec![*true_dest, *false_dest],
        Terminator::Switch { cases, default, .. } => {
            let mut t: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            t.push(*default);
            t
        }
    }
}

/// Apply a closure to every operand slot of an instruction in place.
pub fn for_each_operand_mut(kind: &mut InstKind, mut r: impl FnMut(&mut ValueId)) {
    match kind {
        InstKind::ConstInt(..) | InstKind::ConstFloat(..) |
        InstKind::ConstBool(..) | InstKind::ConstString(..) |
        InstKind::Undef(..) | InstKind::Alloca(..) => {}

        InstKind::IAdd(a, b) | InstKind::ISub(a, b) |
        InstKind::IMul(a, b) | InstKind::IDiv(a, b) |
        InstKind::IMod(a, b) => { r(a); r(b); }
        InstKind::INeg(a) => r(a),

        InstKind::FAdd(a, b) | InstKind::FSub(a, b) |
        InstKind::FMul(a, b) | InstKind::FDiv(a, b) |
        InstKind::FPow(a, b) => { r(a); r(b); }
        InstKind::FNeg(a) | InstKind::FAbs(a) | InstKind::FSqrt(a) => r(a),

        InstKind::ICmp(_, a, b) | InstKind::FCmp(_, a, b) => { r(a); r(b); }

        InstKind::And(a, b) | InstKind::Or(a, b) => { r(a); r(b); }
        InstKind::Not(a) => r(a),

        InstKind::Select(c, t, f) => { r(c); r(t); r(f); }

        InstKind::BitAnd(a, b) | InstKind::BitOr(a, b) |
        InstKind::BitXor(a, b) | InstKind::Shl(a, b) |
        InstKind::LShr(a, b) | InstKind::AShr(a, b) => { r(a); r(b); }
        InstKind::BitNot(a) | InstKind::CountLeadingZeros(a) |
        InstKind::CountTrailingZeros(a) | InstKind::PopCount(a) => r(a),

        InstKind::IntToFloat(v, _) | InstKind::FloatToInt(v, _) |
        InstKind::FloatExtend(v, _) | InstKind::FloatTrunc(v, _) |
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) => r(v),

        InstKind::Load(a) => r(a),
        InstKind::Store(v, a) => { r(v); r(a); }
        InstKind::GetElementPtr(base, idxs) => {
            r(base);
            for i in idxs { r(i); }
        }

        InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
            for a in args { r(a); }
        }

        InstKind::ExtractField(agg, _) => r(agg),
        InstKind::InsertField(agg, _, val) => { r(agg); r(val); }
    }
}

/// Apply a closure to every operand slot of a terminator in place.
pub fn for_each_terminator_operand_mut(
    term: &mut Terminator,
    mut r: impl FnMut(&mut ValueId),
) {
    match term {
        Terminator::Return(None) | Terminator::Unreachable => {}
        Terminator::Return(Some(v)) => r(v),
        Terminator::Branch(_, args) => for a in args { r(a); },
        Terminator::CondBranch { cond, true_args, false_args, .. } => {
            r(cond);
            for a in true_args { r(a); }
            for a in false_args { r(a); }
        }
        Terminator::Switch { selector, .. } => r(selector),
    }
}

/// Replace every use of `old` with `new` across the entire function.
/// Definitions are unaffected — only operand slots in instructions and
/// terminators are rewritten.
pub fn substitute_uses(func: &mut Function, old: ValueId, new: ValueId) {
    let r = |v: &mut ValueId| if *v == old { *v = new; };
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            for_each_operand_mut(&mut inst.kind, r);
        }
        if let Some(term) = &mut block.terminator {
            for_each_terminator_operand_mut(term, r);
        }
    }
}

/// Remove blocks unreachable from the function entry. Returns true if
/// any blocks were dropped.
pub fn prune_unreachable(func: &mut Function) -> bool {
    let mut reachable: HashSet<BlockId> = HashSet::new();
    let mut queue: VecDeque<BlockId> = VecDeque::new();
    queue.push_back(func.entry);
    reachable.insert(func.entry);
    while let Some(bid) = queue.pop_front() {
        let block = func.block(bid);
        if let Some(term) = &block.terminator {
            for tgt in terminator_targets(term) {
                if reachable.insert(tgt) {
                    queue.push_back(tgt);
                }
            }
        }
    }
    let before = func.blocks.len();
    func.blocks.retain(|b| reachable.contains(&b.id));
    func.blocks.len() != before
}
