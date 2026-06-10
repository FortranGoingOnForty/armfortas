//! Code generation.
//!
//! Per-arch backends live in submodules (`arm64/`, `x86/` from x03);
//! each owns its MIR, instruction selection, register allocation, and
//! emitter until x05 shows what the allocator genuinely shares.
//! Target-neutral identifiers live in `shared`.

pub mod arm64;
pub mod shared;

// x03 module reorg: the ARM64 backend moved into `arm64/`. These shims
// preserve the old paths so nothing outside the crate changes; new code
// should reference `codegen::arm64::*` directly.
pub use arm64::abi;
pub use arm64::emit;
pub use arm64::isel;
pub use arm64::linearscan;
pub use arm64::liveness;
pub use arm64::mir;
pub use arm64::peephole;
pub use arm64::regalloc;
pub use arm64::relax_branches;
pub use arm64::tailcall;
