//! x86_64 backend (sprint x03 onward): MIR vocabulary, AT&T/ELF
//! emitter, SysV ABI classifier (x04), and instruction selection plus
//! the two-address conversion pass (x05). Not yet dispatched from
//! `codegen::emit_module` — regalloc and frame layout must land first.

pub mod abi;
pub mod emit;
pub mod isel;
pub mod mir;
pub mod twoaddr;
