//! x86_64 backend (sprint x03 onward): MIR vocabulary and AT&T/ELF
//! emitter. Instruction selection lands in x05, the SysV ABI
//! classifier in x04.

pub mod emit;
pub mod mir;
