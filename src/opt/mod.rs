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
pub mod const_fold;
pub mod const_prop;
pub mod dce;
pub mod cse;

#[allow(unused_imports)]
pub use pass::{Pass, PassManager, PassRunResult};
pub use pipeline::{OptLevel, build_pipeline};
#[allow(unused_imports)]
pub use const_fold::ConstFold;
#[allow(unused_imports)]
pub use const_prop::ConstProp;
#[allow(unused_imports)]
pub use dce::Dce;
#[allow(unused_imports)]
pub use cse::LocalCse;
