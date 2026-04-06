//! SSA-form intermediate representation.
//!
//! Typed IR with block parameters (not phi nodes).
//! Fortran-aware: understands array descriptors, string descriptors,
//! and allocatable semantics.

pub mod types;
pub mod inst;
pub mod builder;
pub mod printer;
pub mod verify;
pub mod walk;
pub mod lower;
