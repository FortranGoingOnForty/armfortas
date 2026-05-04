//! AST → IR lowering.
//!
//! Walks the typed AST and produces SSA IR. Handles variable allocation,
//! expression evaluation, assignments, and runtime calls for I/O.
//!
//! ## Module layout
//!
//! `lower` is undergoing a multi-sprint decomposition (see
//! `.docs/sprints/sprint-04-lower-phase1-extractions.md`). The
//! big body still lives in `core`; focused submodules peel off as
//! the sprint progresses (`ctx` lifted in step 2). Public re-exports
//! preserve the historical `crate::ir::lower::*` API.

mod const_scalar;
mod core;
mod ctx;
mod helpers;
mod init;
mod expr;
mod intrinsic;
mod intrinsic_sub;
mod stmt;
mod unit;

pub use core::*;
pub(crate) use ctx::CharKind;
