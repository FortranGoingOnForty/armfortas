//! Re-exports of IR-walking helpers for the optimizer.
//!
//! Audit B-6: the canonical implementations live in
//! [`crate::ir::walk`]. This module exists so optimizer passes can
//! continue to write `use super::util::...` without each pass needing
//! to reach into `crate::ir::walk` directly. Adding a new helper
//! belongs in `ir/walk.rs`; this file just makes it accessible to
//! the `opt` module tree.

#[allow(unused_imports)]
pub use crate::ir::walk::{
    inst_uses,
    terminator_uses,
    terminator_targets,
    for_each_operand_mut,
    for_each_terminator_operand_mut,
    substitute_uses,
    predecessors,
    compute_dominators,
    find_natural_loops,
    NaturalLoop,
    prune_unreachable,
};
