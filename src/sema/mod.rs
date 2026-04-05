//! Semantic analysis.
//!
//! Symbol tables, scoping, type checking, module resolution,
//! interface validation, and standard conformance checks.

pub mod symtab;
pub mod resolve;
pub mod types;
pub mod validate;
pub mod intrinsic_modules;
