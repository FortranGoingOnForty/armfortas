//! System V AMD64 parameter-passing classifier (sprint x04).
//!
//! Pure data module — no process spawning, no host queries, no driver
//! imports. x05's instruction selection consumes it; the differential
//! harness keeps it honest against clang.
//!
//! Transcribed from the psABI (`.docs/refs/x86-64-ABI/x86-64-ABI/
//! low-level-sys-info.tex`, §"Parameter Passing": "Classification",
//! "Passing", "Returning of Values"). Where this file and that text
//! disagree, this file is wrong.
//!
//! Decided policies (sprint doc):
//! - **Red zone: never used.** Frame allocation always moves rsp; the
//!   128-byte leaf optimization creates bugs that only reproduce under
//!   signal pressure (l09's IEEE work brings signals in scope).
//! - **AL rule: exact count before every external call.** `movb $N,
//!   %al` with N = xmm argument registers used (0–8), from
//!   `X86AbiState::xmm_used`. Internal calls skip it. One 2-byte mov
//!   per external call is cheaper than auditing which externals are
//!   variadic; AL is an upper bound, and exact is the safest bound.

use super::mir::X86Reg;

/// psABI classes. SSEUP/X87* exist for completeness of the merge
/// rules; nothing armfortas lowers produces them today (`long double`
/// has no IrType — noted to x04 from x02; `__m256+` out of scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Integer,
    Sse,
    SseUp,
    X87,
    X87Up,
    ComplexX87,
    NoClass,
    Memory,
}

/// Scalar leaves the classifier understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiField {
    Bool,
    I8,
    I16,
    I32,
    I64,
    I128,
    F32,
    F64,
    Ptr,
}

impl AbiField {
    pub fn size(self) -> u64 {
        match self {
            AbiField::Bool | AbiField::I8 => 1,
            AbiField::I16 => 2,
            AbiField::I32 | AbiField::F32 => 4,
            AbiField::I64 | AbiField::F64 | AbiField::Ptr => 8,
            AbiField::I128 => 16,
        }
    }

    pub fn align(self) -> u64 {
        // Natural alignment; __int128 is 16-byte aligned in memory.
        self.size()
    }

    fn class(self) -> Class {
        match self {
            AbiField::Bool
            | AbiField::I8
            | AbiField::I16
            | AbiField::I32
            | AbiField::I64
            | AbiField::I128
            | AbiField::Ptr => Class::Integer,
            AbiField::F32 | AbiField::F64 => Class::Sse,
        }
    }
}

/// An aggregate as (offset, scalar) leaves. Nested aggregates flatten
/// to leaves before classification — the psABI classifies recursively
/// and flattening is equivalent for the merge rules.
#[derive(Debug, Clone)]
pub struct AbiAggregate {
    pub size: u64,
    pub align: u64,
    pub fields: Vec<(u64, AbiField)>,
}

/// A value in a signature.
#[derive(Debug, Clone)]
pub enum AbiTy {
    Scalar(AbiField),
    /// `_Complex float` / Fortran COMPLEX(4): struct { float re, im } —
    /// 8 bytes, ONE SSE eightbyte. Passed in one xmm register; returned
    /// packed in xmm0's low 64 bits (real in bits 0–31, imag in 32–63).
    /// It is NOT xmm0/xmm1 — that habit comes from AAPCS64 and is the
    /// most likely silent-wrong spot on this ABI.
    ComplexF32,
    /// `_Complex double` / COMPLEX(8): two SSE eightbytes — two xmm
    /// argument registers, xmm0/xmm1 as a return.
    ComplexF64,
    Aggregate(AbiAggregate),
}

impl AbiTy {
    pub fn size(&self) -> u64 {
        match self {
            AbiTy::Scalar(f) => f.size(),
            AbiTy::ComplexF32 => 8,
            AbiTy::ComplexF64 => 16,
            AbiTy::Aggregate(a) => a.size,
        }
    }

    /// Memory alignment when passed on the stack.
    pub fn stack_align(&self) -> u64 {
        match self {
            AbiTy::Scalar(AbiField::I128) => 16,
            AbiTy::Scalar(f) => f.align().max(8),
            AbiTy::ComplexF32 | AbiTy::ComplexF64 => 8,
            AbiTy::Aggregate(a) => a.align.max(8),
        }
    }

    /// The (offset, field) leaves for classification.
    fn leaves(&self) -> Vec<(u64, AbiField)> {
        match self {
            // __int128 classifies as struct { long low, high }.
            AbiTy::Scalar(AbiField::I128) => {
                vec![(0, AbiField::I64), (8, AbiField::I64)]
            }
            AbiTy::Scalar(f) => vec![(0, *f)],
            AbiTy::ComplexF32 => vec![(0, AbiField::F32), (4, AbiField::F32)],
            AbiTy::ComplexF64 => vec![(0, AbiField::F64), (8, AbiField::F64)],
            AbiTy::Aggregate(a) => a.fields.clone(),
        }
    }
}

/// Classification result: MEMORY, or one class per eightbyte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classified {
    Memory,
    Eightbytes(Vec<Class>),
}

/// The psABI classification algorithm over eightbytes.
pub fn classify(ty: &AbiTy) -> Classified {
    let size = ty.size();
    let n_eightbytes = size.div_ceil(8).max(1) as usize;

    // "If the size of an object is larger than eight eightbytes ...
    // it has class MEMORY." (Aggregates >2 eightbytes die in post-merge
    // anyway since we never produce SSEUP, but the rule is kept for
    // fidelity.)
    if n_eightbytes > 8 {
        return Classified::Memory;
    }
    // "... or it contains unaligned fields, it has class MEMORY."
    // Classify from field offsets, not just size: packed BIND(C)
    // structs are MEMORY even when register-sized.
    for (off, f) in ty.leaves() {
        if off % f.align() != 0 {
            return Classified::Memory;
        }
    }

    // Merge field classes per eightbyte.
    let mut classes = vec![Class::NoClass; n_eightbytes];
    for (off, f) in ty.leaves() {
        // A leaf can straddle eightbytes only if unaligned (caught
        // above), except i128's two i64 halves which are exact.
        let idx = (off / 8) as usize;
        let fclass = f.class();
        classes[idx] = merge(classes[idx], fclass);
        // An aligned 8-byte field at offset%8==0 occupies one
        // eightbyte; smaller fields never straddle when aligned.
        debug_assert!(off % 8 + f.size().min(8) <= 8);
        if f.size() > 8 {
            classes[idx + 1] = merge(classes[idx + 1], fclass);
        }
    }

    // Post-merger cleanup, in psABI order.
    if classes.contains(&Class::Memory) {
        return Classified::Memory;
    }
    for i in 0..classes.len() {
        if classes[i] == Class::X87Up && (i == 0 || classes[i - 1] != Class::X87) {
            return Classified::Memory;
        }
    }
    if classes.len() > 2
        && (classes[0] != Class::Sse || classes[1..].iter().any(|c| *c != Class::SseUp))
    {
        return Classified::Memory;
    }
    for i in 0..classes.len() {
        if classes[i] == Class::SseUp
            && (i == 0 || !matches!(classes[i - 1], Class::Sse | Class::SseUp))
        {
            classes[i] = Class::Sse;
        }
    }

    Classified::Eightbytes(classes)
}

/// "If both classes are equal, this is the resulting class. ..."
fn merge(a: Class, b: Class) -> Class {
    use Class::*;
    if a == b {
        return a;
    }
    match (a, b) {
        (NoClass, x) | (x, NoClass) => x,
        (Memory, _) | (_, Memory) => Memory,
        (Integer, _) | (_, Integer) => Integer,
        (X87, _) | (_, X87) | (X87Up, _) | (_, X87Up) | (ComplexX87, _) | (_, ComplexX87) => Memory,
        _ => Sse,
    }
}

/// Argument register sequences (psABI "Passing").
pub const ARG_GP: [X86Reg; 6] = [
    X86Reg::Rdi,
    X86Reg::Rsi,
    X86Reg::Rdx,
    X86Reg::Rcx,
    X86Reg::R8,
    X86Reg::R9,
];
pub const ARG_XMM: [X86Reg; 8] = [
    X86Reg::Xmm0,
    X86Reg::Xmm1,
    X86Reg::Xmm2,
    X86Reg::Xmm3,
    X86Reg::Xmm4,
    X86Reg::Xmm5,
    X86Reg::Xmm6,
    X86Reg::Xmm7,
];

/// Callee-saved registers (psABI Figure "Register Usage"). No xmm
/// register is callee-saved — FP values live across calls must spill,
/// which is x05's allocator problem and the reason the differential
/// matrix has FP-live-across-call rows.
pub const CALLEE_SAVED_GP: [X86Reg; 6] = [
    X86Reg::Rbx,
    X86Reg::Rbp,
    X86Reg::R12,
    X86Reg::R13,
    X86Reg::R14,
    X86Reg::R15,
];

/// Where one argument lives. At most two eightbytes reach registers
/// (post-merge guarantees it absent SSEUP, which nothing produces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X86AbiArgLoc {
    Gp(X86Reg),
    GpPair(X86Reg, X86Reg),
    Xmm(X86Reg),
    XmmPair(X86Reg, X86Reg),
    /// Mixed two-eightbyte aggregates: {i64,f64} → Gp then Xmm, in
    /// eightbyte order.
    GpThenXmm(X86Reg, X86Reg),
    XmmThenGp(X86Reg, X86Reg),
    Stack {
        offset: i64,
        size: i64,
        align: i64,
    },
}

/// Running assignment state for one call's argument list.
#[derive(Debug, Clone, Default)]
pub struct X86AbiState {
    gp_idx: usize,
    xmm_idx: usize,
    stack_offset: i64,
}

impl X86AbiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The AL value for a variadic/external call: xmm argument
    /// registers used so far (0–8).
    pub fn xmm_used(&self) -> u8 {
        self.xmm_idx as u8
    }

    /// Total stack-argument bytes (for the caller's sub rsp).
    pub fn stack_bytes(&self) -> i64 {
        self.stack_offset
    }

    fn push_stack(&mut self, ty: &AbiTy) -> X86AbiArgLoc {
        // "The size of each argument gets rounded up to eightbytes";
        // i128 in memory is 16-byte aligned (psABI "Classification").
        let align = ty.stack_align() as i64;
        self.stack_offset = (self.stack_offset + align - 1) & !(align - 1);
        let offset = self.stack_offset;
        let size = ((ty.size() + 7) & !7) as i64;
        self.stack_offset += size;
        X86AbiArgLoc::Stack {
            offset,
            size,
            align,
        }
    }
}

/// Assign one argument (psABI "Passing"). Left-to-right over the
/// argument list; eightbytes of one argument are all-or-nothing: "if
/// there are no registers available for any eightbyte of an argument,
/// the whole argument is passed on the stack. If registers have
/// already been assigned for some eightbytes of such an argument, the
/// assignments get reverted."
///
/// Deliberately NOT the AAPCS64 lockout: after a reverted argument
/// goes to memory, later arguments still take the remaining registers.
/// A classifier that copies `abi.rs`'s `gp_idx = 8` passes every
/// simple test and breaks only on clang interop with 5+ mixed args.
pub fn classify_abi_arg(ty: &AbiTy, state: &mut X86AbiState) -> X86AbiArgLoc {
    let classes = match classify(ty) {
        Classified::Memory => return state.push_stack(ty),
        Classified::Eightbytes(c) => c,
    };

    let gp_needed = classes.iter().filter(|c| **c == Class::Integer).count();
    let xmm_needed = classes.iter().filter(|c| **c == Class::Sse).count();
    debug_assert!(
        gp_needed + xmm_needed == classes.len(),
        "unexpected class survived post-merge: {:?}",
        classes
    );

    if state.gp_idx + gp_needed > ARG_GP.len() || state.xmm_idx + xmm_needed > ARG_XMM.len() {
        // The revert rule: no tentative assignments stick, and the
        // register file stays open for later arguments.
        return state.push_stack(ty);
    }

    let mut regs = Vec::with_capacity(classes.len());
    for class in &classes {
        match class {
            Class::Integer => {
                regs.push(ARG_GP[state.gp_idx]);
                state.gp_idx += 1;
            }
            Class::Sse => {
                regs.push(ARG_XMM[state.xmm_idx]);
                state.xmm_idx += 1;
            }
            other => unreachable!("post-merge class {:?}", other),
        }
    }

    match (classes.as_slice(), regs.as_slice()) {
        ([Class::Integer], [r]) => X86AbiArgLoc::Gp(*r),
        ([Class::Sse], [r]) => X86AbiArgLoc::Xmm(*r),
        ([Class::Integer, Class::Integer], [a, b]) => X86AbiArgLoc::GpPair(*a, *b),
        ([Class::Sse, Class::Sse], [a, b]) => X86AbiArgLoc::XmmPair(*a, *b),
        ([Class::Integer, Class::Sse], [a, b]) => X86AbiArgLoc::GpThenXmm(*a, *b),
        ([Class::Sse, Class::Integer], [a, b]) => X86AbiArgLoc::XmmThenGp(*a, *b),
        other => unreachable!("argument shape {:?}", other),
    }
}

/// Where a return value lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X86RetLoc {
    /// Register sequence in eightbyte order: INTEGER eightbytes take
    /// rax then rdx; SSE eightbytes take xmm0 then xmm1. One entry per
    /// eightbyte — `ComplexF32` is the one-eightbyte `[Xmm0]` case,
    /// packed (real bits 0–31, imag 32–63).
    Regs(Vec<X86Reg>),
    /// Caller passes a buffer address in rdi as a hidden first
    /// argument (consuming the first GP slot); on return rax carries
    /// that same address — clang callers may legitimately use it.
    Memory,
}

pub fn classify_return(ty: &AbiTy) -> X86RetLoc {
    let classes = match classify(ty) {
        Classified::Memory => return X86RetLoc::Memory,
        Classified::Eightbytes(c) => c,
    };
    const RET_GP: [X86Reg; 2] = [X86Reg::Rax, X86Reg::Rdx];
    const RET_XMM: [X86Reg; 2] = [X86Reg::Xmm0, X86Reg::Xmm1];
    let mut gp = 0;
    let mut xmm = 0;
    let mut regs = Vec::with_capacity(classes.len());
    for class in &classes {
        match class {
            Class::Integer => {
                regs.push(RET_GP[gp]);
                gp += 1;
            }
            Class::Sse => {
                regs.push(RET_XMM[xmm]);
                xmm += 1;
            }
            other => unreachable!("post-merge class {:?}", other),
        }
    }
    X86RetLoc::Regs(regs)
}

/// SysV stack-slot shape for one overflow argument: size rounded to
/// eightbytes, alignment 8 (16 for i128 / 16-aligned aggregates).
/// Deliberately separate from AAPCS64's `abi_stack_layout` and its
/// 2-byte i16 slots — different packing, no sharing.
pub fn sysv_stack_layout(ty: &AbiTy) -> (i64, i64) {
    (((ty.size() + 7) & !7) as i64, ty.stack_align() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use X86Reg::*;

    // Golden classification matrix (x04). Expected values derived from
    // Apple-clang-flavored LLVM 19 on the FreeBSD dev box, 2026-06-10:
    //   cc -S -O1 abiprobe2.c   (value-constructing callers; registers
    //   read off the emitted call sequences — see sprint log)
    // and cross-checked against psABI §3.2.3. Each row notes the
    // observed clang evidence.

    fn agg(size: u64, align: u64, fields: &[(u64, AbiField)]) -> AbiTy {
        AbiTy::Aggregate(AbiAggregate {
            size,
            align,
            fields: fields.to_vec(),
        })
    }

    fn arg(ty: &AbiTy, state: &mut X86AbiState) -> X86AbiArgLoc {
        classify_abi_arg(ty, state)
    }

    #[test]
    fn scalars_take_expected_registers() {
        let mut s = X86AbiState::new();
        // i32, i64, ptr, bool → GP sequence rdi..r9
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::I32), &mut s),
            X86AbiArgLoc::Gp(Rdi)
        );
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::I64), &mut s),
            X86AbiArgLoc::Gp(Rsi)
        );
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::Ptr), &mut s),
            X86AbiArgLoc::Gp(Rdx)
        );
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::Bool), &mut s),
            X86AbiArgLoc::Gp(Rcx)
        );
        // floats take xmm0.. independently of the GP sequence
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::F32), &mut s),
            X86AbiArgLoc::Xmm(Xmm0)
        );
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::F64), &mut s),
            X86AbiArgLoc::Xmm(Xmm1)
        );
        assert_eq!(s.xmm_used(), 2);
    }

    #[test]
    fn i128_takes_a_gp_pair_and_is_16_aligned_in_memory() {
        let mut s = X86AbiState::new();
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::I128), &mut s),
            X86AbiArgLoc::GpPair(Rdi, Rsi)
        );
        assert_eq!(sysv_stack_layout(&AbiTy::Scalar(AbiField::I128)), (16, 16));
    }

    #[test]
    fn two_int_struct_packs_into_one_gp() {
        // clang: t_ii → movabsq $0x200000001, %rdi (ONE register)
        let ty = agg(8, 4, &[(0, AbiField::I32), (4, AbiField::I32)]);
        let mut s = X86AbiState::new();
        assert_eq!(arg(&ty, &mut s), X86AbiArgLoc::Gp(Rdi));
    }

    #[test]
    fn two_f32_struct_packs_into_one_xmm() {
        // clang: t_ff → movsd .LCPI(%rip), %xmm0 (ONE register, packed)
        let ty = agg(8, 4, &[(0, AbiField::F32), (4, AbiField::F32)]);
        let mut s = X86AbiState::new();
        assert_eq!(arg(&ty, &mut s), X86AbiArgLoc::Xmm(Xmm0));
    }

    #[test]
    fn two_f64_struct_takes_xmm_pair() {
        // clang: t_dd → xmm0, xmm1
        let ty = agg(16, 8, &[(0, AbiField::F64), (8, AbiField::F64)]);
        let mut s = X86AbiState::new();
        assert_eq!(arg(&ty, &mut s), X86AbiArgLoc::XmmPair(Xmm0, Xmm1));
    }

    #[test]
    fn mixed_long_double_struct_splits_gp_then_xmm() {
        // clang: t_ld → movl $7, %edi + movsd ..., %xmm0
        let ty = agg(16, 8, &[(0, AbiField::I64), (8, AbiField::F64)]);
        let mut s = X86AbiState::new();
        assert_eq!(arg(&ty, &mut s), X86AbiArgLoc::GpThenXmm(Rdi, Xmm0));
        // and the flipped layout
        let ty = agg(16, 8, &[(0, AbiField::F64), (8, AbiField::I64)]);
        let mut s = X86AbiState::new();
        assert_eq!(arg(&ty, &mut s), X86AbiArgLoc::XmmThenGp(Xmm0, Rdi));
    }

    #[test]
    fn int_float_struct_merges_to_integer() {
        // clang: t_if → movabsq $0x4000000000000007, %rdi — the f32
        // shares the eightbyte with the i32 and INTEGER wins the merge.
        let ty = agg(8, 4, &[(0, AbiField::I32), (4, AbiField::F32)]);
        let mut s = X86AbiState::new();
        assert_eq!(arg(&ty, &mut s), X86AbiArgLoc::Gp(Rdi));
    }

    #[test]
    fn three_eightbyte_struct_is_memory() {
        // clang: t_s3 → movups to (%rsp), callq (memory)
        let ty = agg(
            24,
            8,
            &[(0, AbiField::I64), (8, AbiField::I64), (16, AbiField::I64)],
        );
        assert_eq!(classify(&ty), Classified::Memory);
    }

    #[test]
    fn five_eightbyte_struct_is_memory() {
        let fields: Vec<(u64, AbiField)> = (0..5).map(|i| (i * 8, AbiField::I64)).collect();
        assert_eq!(classify(&agg(40, 8, &fields)), Classified::Memory);
    }

    #[test]
    fn packed_unaligned_struct_is_memory() {
        // clang: t_pk → stores to (%rsp), callq — memory despite being
        // 9 bytes ≈ 2 eightbytes. Unaligned i64 at offset 1.
        let ty = agg(9, 1, &[(0, AbiField::I8), (1, AbiField::I64)]);
        assert_eq!(classify(&ty), Classified::Memory);
    }

    #[test]
    fn complex_f32_is_one_packed_xmm() {
        // clang: t_cf → ONE movsd into xmm0 (real bits 0-31, imag
        // 32-63). NOT xmm0/xmm1 — the AAPCS64 habit this test exists
        // to kill.
        let mut s = X86AbiState::new();
        assert_eq!(arg(&AbiTy::ComplexF32, &mut s), X86AbiArgLoc::Xmm(Xmm0));
        assert_eq!(
            classify_return(&AbiTy::ComplexF32),
            X86RetLoc::Regs(vec![Xmm0])
        );
    }

    #[test]
    fn complex_f64_is_an_xmm_pair() {
        // clang: t_cd → xmm0, xmm1 both ways.
        let mut s = X86AbiState::new();
        assert_eq!(
            arg(&AbiTy::ComplexF64, &mut s),
            X86AbiArgLoc::XmmPair(Xmm0, Xmm1)
        );
        assert_eq!(
            classify_return(&AbiTy::ComplexF64),
            X86RetLoc::Regs(vec![Xmm0, Xmm1])
        );
    }

    #[test]
    fn seventh_integer_arg_overflows_to_stack() {
        let mut s = X86AbiState::new();
        for _ in 0..6 {
            arg(&AbiTy::Scalar(AbiField::I64), &mut s);
        }
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::I64), &mut s),
            X86AbiArgLoc::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
    }

    #[test]
    fn ninth_float_arg_overflows_to_stack() {
        let mut s = X86AbiState::new();
        for _ in 0..8 {
            arg(&AbiTy::Scalar(AbiField::F64), &mut s);
        }
        assert_eq!(s.xmm_used(), 8);
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::F64), &mut s),
            X86AbiArgLoc::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
    }

    #[test]
    fn revert_rule_keeps_register_file_open() {
        // clang (abiprobe3.c, 2026-06-10): five longs take rdi..r8, the
        // {long,long} struct needs two GPs with one left → whole struct
        // to (%rsp) — and the LATER long g takes %r9d. No AAPCS64-style
        // lockout.
        let mut s = X86AbiState::new();
        for _ in 0..5 {
            arg(&AbiTy::Scalar(AbiField::I64), &mut s);
        }
        let two_gp = agg(16, 8, &[(0, AbiField::I64), (8, AbiField::I64)]);
        assert_eq!(
            arg(&two_gp, &mut s),
            X86AbiArgLoc::Stack {
                offset: 0,
                size: 16,
                align: 8
            }
        );
        // The 6th GP register is still live for later arguments.
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::I64), &mut s),
            X86AbiArgLoc::Gp(R9)
        );
    }

    #[test]
    fn returns_use_rax_rdx_and_xmm01() {
        assert_eq!(
            classify_return(&AbiTy::Scalar(AbiField::I64)),
            X86RetLoc::Regs(vec![Rax])
        );
        assert_eq!(
            classify_return(&AbiTy::Scalar(AbiField::I128)),
            X86RetLoc::Regs(vec![Rax, Rdx])
        );
        assert_eq!(
            classify_return(&AbiTy::Scalar(AbiField::F64)),
            X86RetLoc::Regs(vec![Xmm0])
        );
        // {long,double} returns rax + xmm0 (clang t_rld: rax then xmm0)
        let ty = agg(16, 8, &[(0, AbiField::I64), (8, AbiField::F64)]);
        assert_eq!(classify_return(&ty), X86RetLoc::Regs(vec![Rax, Xmm0]));
        // 3 eightbytes → memory: hidden rdi pointer, address back in rax
        let fields: Vec<(u64, AbiField)> = (0..3).map(|i| (i * 8, AbiField::I64)).collect();
        assert_eq!(classify_return(&agg(24, 8, &fields)), X86RetLoc::Memory);
    }

    /// x04 deliverable 2: the descriptor blobs, if ever passed by
    /// value, are MEMORY — so a future by-value experiment fails here,
    /// not in a debugger. They are passed by pointer today
    /// (`by_ref`, src/ir/lower/ctx.rs) and stay that way.
    #[test]
    fn descriptors_by_value_would_be_memory() {
        let array_desc_fields: Vec<(u64, AbiField)> =
            (0..49).map(|i| (i * 8, AbiField::I64)).collect();
        let array_desc = agg(392, 8, &array_desc_fields);
        assert_eq!(classify(&array_desc), Classified::Memory);

        let string_desc = agg(
            32,
            8,
            &[
                (0, AbiField::Ptr),
                (8, AbiField::I64),
                (16, AbiField::I64),
                (24, AbiField::I64),
            ],
        );
        assert_eq!(classify(&string_desc), Classified::Memory);
    }

    #[test]
    fn stack_args_round_to_eightbytes_and_i128_aligns() {
        let mut s = X86AbiState::new();
        for _ in 0..6 {
            arg(&AbiTy::Scalar(AbiField::I64), &mut s);
        }
        // i16 stack slot occupies a full eightbyte (unlike AAPCS64's
        // 2-byte packing).
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::I16), &mut s),
            X86AbiArgLoc::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
        // i128 spilling to the stack-arg area aligns to 16 there.
        assert_eq!(
            arg(&AbiTy::Scalar(AbiField::I128), &mut s),
            X86AbiArgLoc::Stack {
                offset: 16,
                size: 16,
                align: 16
            }
        );
        assert_eq!(s.stack_bytes(), 32);
    }
}
