# Sprint 1: afs-as — ARM64 Instruction Encoding

## Prerequisites
Sprint 0 (workspace exists, refs cloned, ARM64 ISA surveyed)

## Goals
Implement the core of the assembler: the ability to encode ARM64 instructions from a structured Rust representation into their binary (4-byte) machine code form. This is pure bit manipulation — no parsing, no file output yet.

## Deliverables

### 1. Instruction Representation
Define Rust types for ARM64 instructions:

```rust
enum Instruction {
    // Data processing - register
    AddReg { rd: Reg, rn: Reg, rm: Reg, sf: bool },
    SubReg { rd: Reg, rn: Reg, rm: Reg, sf: bool },
    // Data processing - immediate
    AddImm { rd: Reg, rn: Reg, imm12: u16, shift: bool, sf: bool },
    // ...
    // Branching
    B { offset: i32 },
    Bl { offset: i32 },
    BCond { cond: Condition, offset: i32 },
    Cbz { rt: Reg, offset: i32, sf: bool },
    Ret { rn: Reg },
    // Load/Store
    LdrImm { rt: Reg, rn: Reg, offset: i16, size: MemSize },
    StrImm { rt: Reg, rn: Reg, offset: i16, size: MemSize },
    Ldp { rt1: Reg, rt2: Reg, rn: Reg, offset: i16, sf: bool },
    Stp { rt1: Reg, rt2: Reg, rn: Reg, offset: i16, sf: bool },
    // FP/SIMD
    FaddS { rd: Reg, rn: Reg, rm: Reg },
    FaddD { rd: Reg, rn: Reg, rm: Reg },
    // System
    Svc { imm16: u16 },
    Nop,
    Brk { imm16: u16 },
    // ...
}
```

### 2. Register Definitions
```rust
enum Reg {
    X0..X30,   // 64-bit general purpose
    W0..W30,   // 32-bit (lower half of X)
    SP,        // stack pointer (X31 context-dependent)
    XZR, WZR,  // zero registers (X31 context-dependent)
    D0..D31,   // 64-bit FP
    S0..S31,   // 32-bit FP
    Q0..Q31,   // 128-bit SIMD
}
```

### 3. Encoding Functions
Each instruction variant gets an `encode(&self) -> u32` implementation that produces the correct 4-byte ARM64 encoding.

ARM64 instruction encoding is regular — each instruction class has a fixed format:
- **Data processing (register)**: `sf|opc|shift|rm|imm6|rn|rd`
- **Data processing (immediate)**: `sf|opc|sh|imm12|rn|rd`
- **Branch (unconditional)**: `op|imm26`
- **Branch (conditional)**: `cond|imm19|op`
- **Load/store (unsigned offset)**: `size|opc|imm12|rn|rt`
- etc.

Reference: ARM Architecture Reference Manual, C4 (A64 Instruction Set Encoding)

### 4. Instruction Subsets to Encode
Priority order (what the compiler will need first):

**Must have:**
- Arithmetic: ADD, SUB, MUL, SDIV, UDIV (register and immediate forms)
- Logic: AND, ORR, EOR, MVN, TST (register and immediate)
- Shift: LSL, LSR, ASR (register and immediate)
- Move: MOV, MOVZ, MOVK, MOVN (for constant materialization)
- Compare: CMP, CMN, TST
- Branch: B, BL, B.cond, CBZ, CBNZ, TBZ, TBNZ, RET, BR, BLR
- Load/Store: LDR, STR (immediate, register, pre/post-index), LDP, STP
- Address: ADR, ADRP (PC-relative addressing)
- Stack: STP/LDP for push/pop patterns
- System: SVC (syscalls), NOP, BRK

**Should have:**
- FP arithmetic: FADD, FSUB, FMUL, FDIV, FNEG, FABS, FSQRT (single + double)
- FP compare: FCMP, FCCMP
- FP convert: FCVT (between sizes), SCVTF, UCVTF, FCVTZS, FCVTZU (int↔float)
- FP move: FMOV (between GP and FP registers)

**Nice to have (later sprints can add):**
- SIMD/NEON: for vectorized array operations (optimization sprint)
- Atomic: LDADD, SWPAL, etc. (if we ever need them)
- Crypto: (probably never)

## Testing Strategy
This is the most testable sprint in the project. For every instruction:

1. Encode it with our `encode()` function
2. Compare against known-good encoding from `as`:
   ```bash
   echo "add x0, x1, x2" | as -o /dev/stdout | otool -t -v /dev/stdin
   ```
3. Or hardcode known encodings from the ARM manual

Write a comprehensive test matrix:
```rust
#[test]
fn test_add_x0_x1_x2() {
    let inst = Instruction::AddReg { rd: X0, rn: X1, rm: X2, sf: true };
    assert_eq!(inst.encode(), 0x8B020020); // known ARM64 encoding
}
```

Target: 200+ encoding tests covering every instruction we implement.

## Definition of Done
- All "must have" instructions encode correctly
- All "should have" FP instructions encode correctly
- Every instruction has at least one test verifying its encoding against a known-good value
- `cargo test -p afs-as` passes with 200+ tests
- Encoding is bitwise identical to what GNU `as` / Apple `as` produces
