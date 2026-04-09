//! Optimization passes over IR.
//!
//! Constant folding, DCE, CSE, LICM, inlining, loop optimizations,
//! NEON vectorization, and Fortran-specific optimizations.
//!
//! Passes are run via the `PassManager`. The pipeline used for a given
//! invocation is determined by the `OptLevel` selected on the command
//! line (`-O0` through `-Ofast`).

pub mod pass;
pub mod pipeline;
pub mod util;
pub mod const_fold;
pub mod const_prop;
pub mod dce;
pub mod cse;
pub mod strength_reduce;
pub mod licm;
pub mod mem2reg;
pub mod dse;
pub mod lsf;
pub mod unroll;
pub mod loop_tree;
pub mod loop_utils;
pub mod preheader;
pub mod unswitch;
pub mod interchange;
pub mod dep_analysis;
pub mod peel;
pub mod fission;
pub mod fusion;

#[cfg(test)]
mod audit_tests;

// Public surface of the opt module: only the entry points the
// driver actually uses. Audit Cos-2: previously every pass was
// re-exported behind `#[allow(unused_imports)]`, which masked any
// future regressions that orphaned a re-export.
pub use pipeline::{OptLevel, build_pipeline};

// Test-only re-export so audit_tests can refer to passes by their
// short name without the full module path.
#[cfg(test)]
pub use cse::LocalCse;
