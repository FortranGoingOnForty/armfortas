//! ARMFORTAS runtime library (libarmfortas_rt).
//!
//! Provides C-ABI functions called by generated Fortran code:
//! I/O, memory management, string operations, program lifecycle.
//!
//! Built as a static library (.a) linked into every produced binary.

// All public functions in this crate are `extern "C"` FFI entry points called
// from generated assembly. They accept raw pointers by design and cannot be
// marked `unsafe` without breaking the C calling convention contract.
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::missing_safety_doc)]

pub mod descriptor;
pub mod array;
pub mod string;
pub mod format;
pub mod io_system;
mod io;
mod mem;
mod lifecycle;
