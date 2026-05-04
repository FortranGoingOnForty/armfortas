//! AST → IR lowering.
//!
//! Walks the typed AST and produces SSA IR. Handles variable allocation,
//! expression evaluation, assignments, and runtime calls for I/O.
//!
//! ## Module layout
//!
//! `lower` is undergoing a multi-sprint decomposition (see
//! `.docs/sprints/sprint-04-lower-phase1-extractions.md`). Today
//! everything lives in `core` and this module is a thin re-export. As
//! features extract into focused submodules (`ctx`, `const_scalar`,
//! `helpers`, ...) they get re-exported here.

mod core;
pub use core::*;
