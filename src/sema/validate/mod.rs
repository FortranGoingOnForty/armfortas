//! Semantic validation pass.
//!
//! Sprint 13 split this module into a directory:
//!  * `core.rs` — entry points and the bulk of statement-/decl-level checks.
//!  * `pure_elemental.rs` — pure / elemental procedure constraints.
//!  * `allocatable.rs` — `allocatable` / `pointer` integrity checks.
//!  * `pointer.rs` — pointer-target validation for `=>` assignments.
//!
//! Public re-exports below preserve the previous flat surface
//! (`crate::sema::validate::validate_file`, `is_intrinsic_name`,
//! `FortranStandard`, ...) so the rest of the compiler isn't disturbed.

mod allocatable;
mod core;
mod pointer;
mod procedure;
mod pure_elemental;

pub use core::*;
