//! Cross-block load-store forwarding.
//!
//! Extends local LSF across basic block boundaries. For each load,
//! walks up the dominator tree looking for a store to the same address
//! with no intervening aliasing store. Uses the alias analysis oracle
//! to disambiguate pointers.
//!
//! ```text
//! block A (dominates B):
//!     store %val → %ptr
//! block B:
//!     %r = load %ptr        → %r = %val (forwarded)
//! ```

use std::collections::HashMap;
use crate::ir::inst::*;
use crate::ir::walk::{compute_immediate_dominators, dominator_tree_children, inst_uses};
use super::alias::{self, AliasResult};
use super::pass::Pass;

pub struct GlobalLsf;

impl Pass for GlobalLsf {
    fn name(&self) -> &'static str { "global-lsf" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if global_lsf_function(func) { changed = true; }
        }
        changed
    }
}

fn global_lsf_function(func: &mut Function) -> bool {
    let idoms = compute_immediate_dominators(func);
    let children = dominator_tree_children(&idoms);

    // Collect all stores in the function: (block_id, ptr, stored_value).
    let stores: Vec<(BlockId, ValueId, ValueId)> = {
        let mut s = Vec::new();
        for block in &func.blocks {
            for inst in &block.insts {
                if let InstKind::Store(val, ptr) = &inst.kind {
                    s.push((block.id, *ptr, *val));
                }
            }
        }
        s
    };

    // For each load, try to find a dominating store to forward from.
    let mut replacements: HashMap<ValueId, ValueId> = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::Load(ptr) = &inst.kind {
                // Find a dominating store to the same address.
                if let Some(forwarded_val) = find_dominating_store(
                    func, &idoms, block.id, *ptr, &stores,
                ) {
                    replacements.insert(inst.id, forwarded_val);
                }
            }
        }
    }

    if replacements.is_empty() { return false; }

    // Apply replacements.
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.kind = super::loop_utils::remap_inst_kind(&inst.kind, &replacements);
        }
        if let Some(ref mut term) = block.terminator {
            let new_term = super::loop_utils::remap_terminator(
                term,
                &HashMap::new(),
                &replacements,
            );
            *term = new_term;
        }
    }

    true
}

/// Walk up the dominator tree from `load_block` looking for a store
/// to `load_ptr` with no intervening aliasing store.
fn find_dominating_store(
    func: &Function,
    idoms: &HashMap<BlockId, BlockId>,
    load_block: BlockId,
    load_ptr: ValueId,
    stores: &[(BlockId, ValueId, ValueId)],
) -> Option<ValueId> {
    // First check the load's own block (before the load).
    let block = func.block(load_block);
    let mut last_store_val = None;
    for inst in &block.insts {
        if inst.id == ValueId(u32::MAX) { continue; } // skip sentinels
        if let InstKind::Store(val, ptr) = &inst.kind {
            match alias::query(func, *ptr, load_ptr) {
                AliasResult::MustAlias => {
                    last_store_val = Some(*val);
                }
                AliasResult::MayAlias => {
                    // Might clobber — reset.
                    last_store_val = None;
                }
                AliasResult::NoAlias => {
                    // Doesn't affect our load — continue.
                }
            }
        }
        if let InstKind::Load(ptr) = &inst.kind {
            if *ptr == load_ptr && inst.id != ValueId(u32::MAX) {
                // This IS the load we're trying to forward. Stop scanning.
                if let Some(val) = last_store_val {
                    return Some(val);
                }
                break;
            }
        }
        // Calls may clobber memory.
        if matches!(&inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)) {
            last_store_val = None;
        }
    }

    // Before checking dominators, verify the load_block itself has no
    // clobbers (calls or aliasing stores) before the load. If it does,
    // cross-block forwarding is unsafe — the clobber invalidates any
    // store from a dominating block.
    let block = func.block(load_block);
    let mut load_block_has_clobber = false;
    for inst in &block.insts {
        // Stop at the load itself.
        if let InstKind::Load(ptr) = &inst.kind {
            if *ptr == load_ptr { break; }
        }
        // Any call before the load is a clobber (LLVM MemorySSA principle:
        // every call is a MemoryDef unless proven NoModRef).
        if matches!(&inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)) {
            load_block_has_clobber = true;
            break;
        }
        // Any aliasing store before the load is a clobber.
        if let InstKind::Store(_, ptr) = &inst.kind {
            if matches!(alias::query(func, *ptr, load_ptr), AliasResult::MayAlias | AliasResult::MustAlias) {
                load_block_has_clobber = true;
                break;
            }
        }
    }
    if load_block_has_clobber { return None; }

    // Walk up dominator tree looking for stores in dominating blocks.
    let mut current = load_block;
    while let Some(&idom) = idoms.get(&current) {
        if idom == current { break; } // entry
        current = idom;

        // Scan the dominating block for stores to our address.
        let dom_block = func.block(current);
        let mut found_store = None;
        let mut clobbered = false;

        for inst in &dom_block.insts {
            if let InstKind::Store(val, ptr) = &inst.kind {
                match alias::query(func, *ptr, load_ptr) {
                    AliasResult::MustAlias => {
                        found_store = Some(*val);
                    }
                    AliasResult::MayAlias => {
                        found_store = None;
                        clobbered = true;
                    }
                    AliasResult::NoAlias => {}
                }
            }
            if matches!(&inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)) {
                found_store = None;
                clobbered = true;
            }
        }

        if let Some(val) = found_store {
            // Found a dominating store with no subsequent clobber in that block.
            // But we need to verify no clobber on the path from idom to load_block.
            // Conservative: only forward if the dominating block is the
            // immediate dominator (one level up). Multi-level requires
            // path-sensitive analysis.
            if current == *idoms.get(&load_block).unwrap_or(&load_block) {
                return Some(val);
            }
        }

        if clobbered { break; }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;

    #[test]
    fn global_lsf_no_op_on_empty() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = GlobalLsf;
        assert!(!pass.run(&mut m));
    }
}
