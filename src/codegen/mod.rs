//! ARM64 code generation.
//!
//! Instruction selection, register allocation, stack frame layout,
//! and emission of machine instructions for the afs-as assembler.

pub mod mir;
pub mod isel;
pub mod liveness;
pub mod regalloc;
pub mod linearscan;
pub mod emit;
pub mod peephole;
pub mod tailcall;
