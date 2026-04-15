# Sprint 17: Codegen — Instruction Selection

## Prerequisites
Sprint 3 (assembler), Sprint 15 (IR basics)

## Goals
Begin the backend: translate IR instructions into ARM64 machine instructions. This sprint covers the "easy" mappings — arithmetic, comparisons, loads, stores, constant materialization. We don't tackle register allocation yet — we use a naive strategy (one virtual register per IR value, spill everything) to get correct code first.

## Deliverables

### 1. Machine IR (MIR)
An intermediate step between our SSA IR and final ARM64 instructions:

```rust
struct MachineFunction {
    name: String,
    blocks: Vec<MachineBlock>,
    frame: StackFrame,
    vregs: Vec<VReg>,        // virtual registers
}

struct MachineBlock {
    id: MBlockId,
    instructions: Vec<MachineInst>,
}

struct MachineInst {
    opcode: ArmOpcode,
    operands: Vec<MachineOperand>,
}

enum MachineOperand {
    VReg(VRegId),
    PhysReg(Reg),
    Imm(i64),
    FrameSlot(i32),          // stack frame offset
    GlobalRef(String),        // reference to global symbol
    BlockRef(MBlockId),
    Extern(String),           // external symbol name
}
```

### 2. Instruction Selection Patterns

**Integer arithmetic:**
```
IR: %c = iadd %a, %b
ARM64: add Xd, Xn, Xm      (64-bit)
       add Wd, Wn, Wm       (32-bit)

IR: %c = iadd %a, const(small)
ARM64: add Xd, Xn, #imm12   (if fits in 12 bits)

IR: %c = imul %a, %b
ARM64: mul Xd, Xn, Xm

IR: %c = idiv %a, %b
ARM64: sdiv Xd, Xn, Xm      (signed)
```

**Float arithmetic:**
```
IR: %c = fadd %a, %b
ARM64: fadd Dd, Dn, Dm       (double)
       fadd Sd, Sn, Sm       (single)
```

**Comparisons:**
```
IR: %r = icmp.eq %a, %b
ARM64: cmp Xn, Xm
       cset Xd, eq

IR: %r = fcmp.lt %a, %b
ARM64: fcmp Dn, Dm
       cset Xd, mi           (less than → minus flag)
```

**Constants:**
```
IR: %x = const_int 42
ARM64: mov Xd, #42           (if fits in 16 bits)

IR: %x = const_int 0x12345678
ARM64: movz Xd, #0x5678      (lower 16)
       movk Xd, #0x1234, lsl #16  (next 16)

IR: %x = const_float 3.14
ARM64: adrp Xd, .Lconst@PAGE
       ldr Dd, [Xd, .Lconst@PAGEOFF]   (load from constant pool)
```

**Memory:**
```
IR: %addr = alloca i32
MIR: (allocate frame slot, record offset)

IR: store %val, %addr
ARM64: str Xn, [Xm, #offset]

IR: %val = load %addr
ARM64: ldr Xd, [Xm, #offset]
```

### 3. Stack Frame Layout
```
┌──────────────────┐  ← SP at function entry
│ Saved LR (x30)   │
│ Saved FP (x29)   │
├──────────────────┤  ← FP (x29) points here
│ Local var 1       │
│ Local var 2       │
│ ...               │
│ Spill slots       │
├──────────────────┤
│ Outgoing args     │  (for calls this function makes)
├──────────────────┤  ← SP during function body
```

Frame setup:
```asm
; Prologue
stp x29, x30, [sp, #-FRAME_SIZE]!
mov x29, sp

; Epilogue
ldp x29, x30, [sp], #FRAME_SIZE
ret
```

FRAME_SIZE must be 16-byte aligned (Apple ARM64 requirement).

### 4. Constant Pool
Floating-point constants and large integer constants go into a constant pool (in the __DATA section or as PC-relative literals):
```rust
struct ConstantPool {
    entries: Vec<ConstPoolEntry>,
}

enum ConstPoolEntry {
    F32(f32),
    F64(f64),
    I64(i64),
    Bytes(Vec<u8>),    // string literals
}
```

### 5. Naive Register Strategy (Pre-Allocation)
For this sprint, every IR value gets a virtual register. Before register allocation (Sprint 21), we use a simple strategy:
- Every virtual register is spilled to a stack slot
- Load before use, store after definition
- This produces correct but terrible code

The point: get correct instruction selection first, optimize register usage later.

## Testing Strategy

### Instruction Selection Unit Tests
For each IR instruction kind, verify the correct ARM64 instruction(s) are selected:
```rust
#[test]
fn test_iadd_selection() {
    let ir = make_iadd(vreg(0), vreg(1));
    let mir = select(ir);
    assert_matches!(mir.opcode, ArmOpcode::Add);
}
```

### Constant Materialization Tests
- Small immediates → single MOV
- 16-bit values → MOVZ
- 32-bit values → MOVZ + MOVK
- 64-bit values → MOVZ + 3x MOVK
- Float → constant pool load

### Stack Frame Tests
- Verify frame size is 16-byte aligned
- Verify prologue saves LR and FP
- Verify epilogue restores and returns
- Verify local variable offsets are correct

### End-to-End (IR → Assembly Text)
Lower IR for simple programs, emit assembly text (`-S` flag), verify it assembles with Apple `as` and produces correct Mach-O.

## Definition of Done
- All IR arithmetic instructions select to correct ARM64 instructions
- All comparisons select correctly (with correct condition codes)
- Loads and stores select correctly (with frame slot offsets)
- Constant materialization works for all sizes
- Stack frame layout correct with 16-byte alignment
- Constant pool for floating-point literals works
- Function prologue/epilogue correct
- Generated assembly assembles without errors (verified with both `as` and `afs-as`)
- `cargo test` codegen tests pass
