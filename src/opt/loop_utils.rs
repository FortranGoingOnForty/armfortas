//! Shared loop analysis utilities.
//!
//! Promotes preheader finding (from licm.rs) and constant resolution
//! (from unroll.rs) into public shared functions so all loop passes
//! use the same logic.

use std::collections::{HashMap, HashSet};
use crate::ir::inst::*;
use crate::ir::walk::NaturalLoop;

/// Find the unique preheader for a natural loop, if one exists.
///
/// Returns `Some(preheader_id)` only when:
///  * the header has exactly one predecessor outside the loop body,
///  * that predecessor's terminator is an unconditional `Branch` to
///    the header, and
///  * the preheader is not itself the header.
pub fn find_preheader(
    func: &Function,
    lp: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<BlockId> {
    let header_preds = preds.get(&lp.header)?;
    let mut outside: Vec<BlockId> = header_preds
        .iter()
        .copied()
        .filter(|p| !lp.body.contains(p))
        .collect();
    outside.sort_by_key(|b| b.0);
    outside.dedup();
    if outside.len() != 1 { return None; }
    let ph = outside[0];
    if ph == lp.header { return None; }
    let ph_block = func.block(ph);
    match &ph_block.terminator {
        Some(Terminator::Branch(dest, _)) if *dest == lp.header => Some(ph),
        _ => None,
    }
}

/// Resolve a ValueId to a compile-time constant integer, if it was
/// produced by a `ConstInt` instruction.
pub fn resolve_const_int(func: &Function, vid: ValueId) -> Option<i64> {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.id == vid {
                if let InstKind::ConstInt(v, _) = inst.kind {
                    return i64::try_from(v).ok();
                }
                return None;
            }
        }
    }
    None
}

/// Collect all ValueIds defined inside a loop body (block params +
/// instruction results).
pub fn loop_defined_values(func: &Function, lp: &NaturalLoop) -> HashSet<ValueId> {
    let mut defs = HashSet::new();
    for &bid in &lp.body {
        let block = func.block(bid);
        for bp in &block.params { defs.insert(bp.id); }
        for inst in &block.insts { defs.insert(inst.id); }
    }
    defs
}

// ---------------------------------------------------------------------------
// Loop cloning and value remapping (shared by unswitching, fission, fusion)
// ---------------------------------------------------------------------------

/// Clone all blocks in a loop body, returning a block-ID mapping
/// (old → new) and the list of new block IDs.
pub fn clone_loop(
    func: &mut Function,
    lp: &NaturalLoop,
) -> (HashMap<BlockId, BlockId>, Vec<BlockId>) {
    let mut block_map: HashMap<BlockId, BlockId> = HashMap::new();
    let mut new_blocks = Vec::new();

    let body_sorted: Vec<BlockId> = {
        let mut v: Vec<BlockId> = lp.body.iter().copied().collect();
        v.sort_by_key(|b| b.0);
        v
    };
    for &old_id in &body_sorted {
        let old_name = &func.block(old_id).name;
        let new_id = func.create_block(&format!("{}_clone", old_name));
        block_map.insert(old_id, new_id);
        new_blocks.push(new_id);
    }

    let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();

    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_params: Vec<BlockParam> = func.block(old_id).params.clone();
        for bp in &old_params {
            let new_vid = func.next_value_id();
            func.register_type(new_vid, bp.ty.clone());
            val_map.insert(bp.id, new_vid);
            func.block_mut(new_id).params.push(BlockParam {
                id: new_vid,
                ty: bp.ty.clone(),
            });
        }
    }

    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_insts: Vec<Inst> = func.block(old_id).insts.clone();
        for inst in &old_insts {
            let new_vid = func.next_value_id();
            func.register_type(new_vid, inst.ty.clone());
            val_map.insert(inst.id, new_vid);
            let new_kind = remap_inst_kind(&inst.kind, &val_map);
            func.block_mut(new_id).insts.push(Inst {
                id: new_vid,
                kind: new_kind,
                ty: inst.ty.clone(),
                span: inst.span,
            });
        }
    }

    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_term = func.block(old_id).terminator.clone();
        if let Some(term) = old_term {
            let new_term = remap_terminator(&term, &block_map, &val_map);
            func.block_mut(new_id).terminator = Some(new_term);
        }
    }

    (block_map, new_blocks)
}

/// Build a value map from original loop blocks → cloned loop blocks.
pub fn build_value_map(
    func: &Function,
    lp: &NaturalLoop,
    block_map: &HashMap<BlockId, BlockId>,
) -> HashMap<ValueId, ValueId> {
    let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();
    let body_sorted: Vec<BlockId> = {
        let mut v: Vec<BlockId> = lp.body.iter().copied().collect();
        v.sort_by_key(|b| b.0);
        v
    };
    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_block = func.block(old_id);
        let new_block = func.block(new_id);
        for (old_bp, new_bp) in old_block.params.iter().zip(new_block.params.iter()) {
            val_map.insert(old_bp.id, new_bp.id);
        }
        for (old_inst, new_inst) in old_block.insts.iter().zip(new_block.insts.iter()) {
            val_map.insert(old_inst.id, new_inst.id);
        }
    }
    val_map
}

/// Remap all ValueId operands in an InstKind. Values not in the map
/// are left unchanged (they're defined outside the cloned region).
pub fn remap_inst_kind(kind: &InstKind, map: &HashMap<ValueId, ValueId>) -> InstKind {
    let r = |v: &ValueId| *map.get(v).unwrap_or(v);
    match kind {
        InstKind::ConstInt(v, w)     => InstKind::ConstInt(*v, *w),
        InstKind::ConstFloat(v, w)   => InstKind::ConstFloat(*v, *w),
        InstKind::ConstBool(v)       => InstKind::ConstBool(*v),
        InstKind::ConstString(v)     => InstKind::ConstString(v.clone()),
        InstKind::Undef(t)           => InstKind::Undef(t.clone()),
        InstKind::GlobalAddr(s)      => InstKind::GlobalAddr(s.clone()),
        InstKind::IAdd(a, b)  => InstKind::IAdd(r(a), r(b)),
        InstKind::ISub(a, b)  => InstKind::ISub(r(a), r(b)),
        InstKind::IMul(a, b)  => InstKind::IMul(r(a), r(b)),
        InstKind::IDiv(a, b)  => InstKind::IDiv(r(a), r(b)),
        InstKind::IMod(a, b)  => InstKind::IMod(r(a), r(b)),
        InstKind::INeg(a)     => InstKind::INeg(r(a)),
        InstKind::FAdd(a, b)  => InstKind::FAdd(r(a), r(b)),
        InstKind::FSub(a, b)  => InstKind::FSub(r(a), r(b)),
        InstKind::FMul(a, b)  => InstKind::FMul(r(a), r(b)),
        InstKind::FDiv(a, b)  => InstKind::FDiv(r(a), r(b)),
        InstKind::FNeg(a)     => InstKind::FNeg(r(a)),
        InstKind::FAbs(a)     => InstKind::FAbs(r(a)),
        InstKind::FSqrt(a)    => InstKind::FSqrt(r(a)),
        InstKind::FPow(a, b)  => InstKind::FPow(r(a), r(b)),
        InstKind::ICmp(op, a, b) => InstKind::ICmp(*op, r(a), r(b)),
        InstKind::FCmp(op, a, b) => InstKind::FCmp(*op, r(a), r(b)),
        InstKind::And(a, b) => InstKind::And(r(a), r(b)),
        InstKind::Or(a, b)  => InstKind::Or(r(a), r(b)),
        InstKind::Not(a)    => InstKind::Not(r(a)),
        InstKind::Select(c, t, f) => InstKind::Select(r(c), r(t), r(f)),
        InstKind::BitAnd(a, b)           => InstKind::BitAnd(r(a), r(b)),
        InstKind::BitOr(a, b)            => InstKind::BitOr(r(a), r(b)),
        InstKind::BitXor(a, b)           => InstKind::BitXor(r(a), r(b)),
        InstKind::BitNot(a)              => InstKind::BitNot(r(a)),
        InstKind::Shl(a, b)              => InstKind::Shl(r(a), r(b)),
        InstKind::LShr(a, b)             => InstKind::LShr(r(a), r(b)),
        InstKind::AShr(a, b)             => InstKind::AShr(r(a), r(b)),
        InstKind::CountLeadingZeros(a)   => InstKind::CountLeadingZeros(r(a)),
        InstKind::CountTrailingZeros(a)  => InstKind::CountTrailingZeros(r(a)),
        InstKind::PopCount(a)            => InstKind::PopCount(r(a)),
        InstKind::IntToFloat(a, w)    => InstKind::IntToFloat(r(a), *w),
        InstKind::FloatToInt(a, w)    => InstKind::FloatToInt(r(a), *w),
        InstKind::FloatExtend(a, w)   => InstKind::FloatExtend(r(a), *w),
        InstKind::FloatTrunc(a, w)    => InstKind::FloatTrunc(r(a), *w),
        InstKind::IntExtend(a, w, s)  => InstKind::IntExtend(r(a), *w, *s),
        InstKind::IntTrunc(a, w)      => InstKind::IntTrunc(r(a), *w),
        InstKind::PtrToInt(a)         => InstKind::PtrToInt(r(a)),
        InstKind::IntToPtr(a, ty)     => InstKind::IntToPtr(r(a), ty.clone()),
        InstKind::Alloca(t)  => InstKind::Alloca(t.clone()),
        InstKind::Load(a)    => InstKind::Load(r(a)),
        InstKind::Store(v, p) => InstKind::Store(r(v), r(p)),
        InstKind::GetElementPtr(base, idxs) =>
            InstKind::GetElementPtr(r(base), idxs.iter().map(&r).collect()),
        InstKind::Call(f, args) =>
            InstKind::Call(f.clone(), args.iter().map(&r).collect()),
        InstKind::RuntimeCall(f, args) =>
            InstKind::RuntimeCall(f.clone(), args.iter().map(&r).collect()),
        InstKind::ExtractField(v, idx)     => InstKind::ExtractField(r(v), *idx),
        InstKind::InsertField(v, idx, fld) => InstKind::InsertField(r(v), *idx, r(fld)),
    }
}

/// Remap block targets and value operands in a terminator.
pub fn remap_terminator(
    term: &Terminator,
    block_map: &HashMap<BlockId, BlockId>,
    val_map: &HashMap<ValueId, ValueId>,
) -> Terminator {
    let rb = |b: &BlockId| *block_map.get(b).unwrap_or(b);
    let rv = |v: &ValueId| *val_map.get(v).unwrap_or(v);
    let rvs = |vs: &[ValueId]| -> Vec<ValueId> { vs.iter().map(&rv).collect() };
    match term {
        Terminator::Return(v) => Terminator::Return(v.map(|x| rv(&x))),
        Terminator::Branch(dest, args) => Terminator::Branch(rb(dest), rvs(args)),
        Terminator::CondBranch { cond, true_dest, true_args, false_dest, false_args } =>
            Terminator::CondBranch {
                cond: rv(cond),
                true_dest: rb(true_dest),
                true_args: rvs(true_args),
                false_dest: rb(false_dest),
                false_args: rvs(false_args),
            },
        Terminator::Switch { selector, default, cases } =>
            Terminator::Switch {
                selector: rv(selector),
                default: rb(default),
                cases: cases.iter().map(|(v, d)| (*v, rb(d))).collect(),
            },
        Terminator::Unreachable => Terminator::Unreachable,
    }
}
