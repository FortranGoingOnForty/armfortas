//! ARM64 code generation.
//!
//! Instruction selection, register allocation, stack frame layout,
//! and emission of machine instructions for the afs-as assembler.

pub mod emit;
pub mod isel;
pub mod linearscan;
pub mod liveness;
pub mod mir;
pub mod peephole;
pub mod regalloc;
pub mod relax_branches;
pub mod tailcall;
