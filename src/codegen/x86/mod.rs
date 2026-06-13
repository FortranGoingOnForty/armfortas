//! x86_64 backend (sprint x03 onward): MIR vocabulary, AT&T/ELF
//! emitter, SysV ABI classifier (x04), instruction selection, the
//! two-address conversion pass, and naive register allocation (x05).
//! Dispatched from `codegen::emit_module` since x06; the O2+ peephole
//! landed in x09.

pub mod abi;
pub mod emit;
pub mod isel;
pub mod liveness;
pub mod mir;
pub mod peephole;
pub mod regalloc;
pub mod twoaddr;
