//! Shared loop analysis utilities.
//!
//! Promotes preheader finding (from licm.rs) and constant resolution
//! (from unroll.rs) into public shared functions so all loop passes
//! use the same logic.

use std::collections::HashMap;
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
                    return Some(v);
                }
                return None;
            }
        }
    }
    None
}

/// Collect all ValueIds defined inside a loop body (block params +
/// instruction results).
pub fn loop_defined_values(func: &Function, lp: &NaturalLoop) -> std::collections::HashSet<ValueId> {
    let mut defs = std::collections::HashSet::new();
    for &bid in &lp.body {
        let block = func.block(bid);
        for bp in &block.params { defs.insert(bp.id); }
        for inst in &block.insts { defs.insert(inst.id); }
    }
    defs
}
