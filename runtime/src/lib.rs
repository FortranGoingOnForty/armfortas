//! ARMFORTAS runtime library (libarmfortas_rt).
//!
//! Provides C-ABI functions called by generated Fortran code:
//! I/O, memory management, string operations, program lifecycle.
//!
//! Built as a static library (.a) linked into every produced binary.

pub mod descriptor;
pub mod array;
pub mod string;
pub mod io_system;
mod io;
mod mem;
mod lifecycle;
