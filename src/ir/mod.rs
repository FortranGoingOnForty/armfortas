//! SSA-form intermediate representation.
//!
//! Typed IR with block parameters (not phi nodes).
//! Fortran-aware: understands array descriptors, string descriptors,
//! and allocatable semantics.

pub mod builder;
pub mod inst;
pub mod lower;
pub mod printer;
pub mod types;
pub mod verify;
pub mod walk;
