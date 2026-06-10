//! ARM64 backend: instruction selection, register allocation, stack
//! frame layout, and emission of assembly text for the afs-as
//! assembler (Mach-O conventions throughout).

pub mod abi;
pub mod emit;
pub mod isel;
pub mod linearscan;
pub mod liveness;
pub mod mir;
pub mod peephole;
pub mod regalloc;
pub mod relax_branches;
pub mod tailcall;
