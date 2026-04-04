//! ARMFORTAS runtime library (libarmfortas_rt).
//!
//! Provides C-ABI functions called by generated Fortran code:
//! I/O, memory management, string operations, program lifecycle.
//!
//! Built as a static library (.a) linked into every produced binary.

mod io;
mod mem;
mod lifecycle;
