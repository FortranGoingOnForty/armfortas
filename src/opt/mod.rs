//! Optimization passes over IR.
//!
//! Constant folding, DCE, CSE, LICM, inlining, loop optimizations,
//! NEON vectorization, and Fortran-specific optimizations.
//!
//! Passes are run via the `PassManager`. The pipeline used for a given
//! invocation is determined by the `OptLevel` selected on the command
//! line (`-O0` through `-Ofast`).

pub mod alias;
pub mod bce;
pub mod call_resolve;
pub mod callgraph;
pub mod const_arg;
pub mod const_fold;
pub mod const_prop;
pub mod cse;
pub mod dce;
pub mod dead_arg;
pub mod dead_func;
pub mod dep_analysis;
pub mod dse;
pub mod fast_math;
pub mod fission;
pub mod fusion;
pub mod global_lsf;
pub mod gvn;
pub mod inline;
pub mod interchange;
pub mod jump_thread;
pub mod licm;
pub mod loop_tree;
pub mod loop_utils;
pub mod lsf;
pub mod mem2reg;
pub mod neon_vectorize;
pub mod pass;
pub mod peel;
pub mod pipeline;
pub mod preheader;
pub mod return_prop;
pub mod simplify_cfg;
pub mod sccp;
pub mod sroa;
pub mod strength_reduce;
pub mod unroll;
pub mod unswitch;
pub mod util;
pub mod vectorize;

#[cfg(test)]
mod audit_tests;

// Public surface of the opt module: only the entry points the
// driver actually uses. Audit Cos-2: previously every pass was
// re-exported behind `#[allow(unused_imports)]`, which masked any
// future regressions that orphaned a re-export.
pub use pipeline::{build_i128_pipeline, build_pipeline, OptLevel};

// Test-only re-export so audit_tests can refer to passes by their
// short name without the full module path.
#[cfg(test)]
pub use cse::LocalCse;
