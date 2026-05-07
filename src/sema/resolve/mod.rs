//! Symbol resolution pass.
//!
//! Sprint 14 split this module into a directory:
//!  * `core.rs` — entry points and shared helpers.
//!  * `use_resolution.rs` — USE statement resolution and rename handling.
//!  * `type_resolution.rs` — type-spec resolution and derived-type body.
//!  * `statement_functions.rs` — F77 statement-function detection.
//!
//! Public re-exports preserve the previous flat surface
//! (`crate::sema::resolve::resolve_file`, ...).

mod core;
mod statement_functions;
mod type_resolution;
mod use_resolution;

pub use core::*;
