//! Re-exports of IR-walking helpers for the optimizer.
//!
//! Audit B-6: the canonical implementations live in
//! [`crate::ir::walk`]. This module exists so optimizer passes can
//! continue to write `use super::util::...` without each pass needing
//! to reach into `crate::ir::walk` directly. Adding a new helper
//! belongs in `ir/walk.rs`; this file just makes it accessible to
//! the `opt` module tree.

// `dominator_tree_preorder` lives in `crate::ir::walk` and is tested
// there but no optimizer pass currently consumes it (mem2reg walks
// the dominator tree itself via `dominator_tree_children`). Drop the
// re-export until something actually needs it — the audit (N13)
// flagged it as dead code from this module's perspective.
#[allow(unused_imports)]
pub use crate::ir::walk::{
    compute_dominance_frontiers, compute_dominators, compute_immediate_dominators,
    dominator_tree_children, find_natural_loops, for_each_operand_mut,
    for_each_terminator_operand_mut, inst_uses, predecessors, prune_unreachable, substitute_uses,
    terminator_targets, terminator_uses, NaturalLoop,
};
