//! Target-neutral codegen identifiers (x03). Both backends use these;
//! everything else — instructions, operands, registers, functions —
//! stays per-backend until x05 shows what the allocator actually
//! shares.

/// Virtual register identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VRegId(pub u32);

/// Machine basic-block identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MBlockId(pub u32);
