# Sprint 21: Codegen — Register Allocation

## Prerequisites
Sprint 20 (calling convention)

## Goals
Replace the naive "spill everything" register strategy with a real register allocator. This is the single biggest performance improvement we can make in the backend — the difference between every value living on the stack vs. most values living in registers.

## Deliverables

### 1. Liveness Analysis
Before allocating registers, compute which virtual registers are "live" (still needed) at each point:

```rust
struct LivenessInfo {
    // For each instruction, which vregs are live at entry and exit
    live_in: HashMap<InstId, HashSet<VRegId>>,
    live_out: HashMap<InstId, HashSet<VRegId>>,
    // For each vreg, where it's defined and last used
    intervals: HashMap<VRegId, LiveInterval>,
}

struct LiveInterval {
    vreg: VRegId,
    start: InstId,
    end: InstId,
    reg_class: RegClass,     // GPR or FPR
    uses: Vec<InstId>,
}
```

Computed via backward dataflow analysis over the CFG.

### 2. Linear Scan Register Allocator
We implement linear scan (not graph coloring) — it's simpler, faster to compile, and produces good-enough code for most programs:

Algorithm:
1. Sort live intervals by start point
2. Walk intervals in order
3. For each interval, try to assign a physical register:
   - Check if any register is available (no active interval conflicts)
   - If yes, assign it
   - If no, spill the interval with the furthest end point (or the current one if it ends first)
4. For spilled intervals, allocate a stack slot and insert load/store instructions

```rust
struct LinearScan {
    active: Vec<Allocation>,      // currently assigned intervals
    available: [Vec<Reg>; 2],     // free registers per class [GPR, FPR]
}

struct Allocation {
    interval: LiveInterval,
    reg: Reg,
}
```

### 3. Register Classes
ARM64 has two main register classes:
- **GPR**: x0-x30 (or w0-w30 for 32-bit). 31 registers, but:
  - x29 = frame pointer (reserved)
  - x30 = link register (reserved, saved/restored)
  - x18 = platform reserved on Apple (do not use!)
  - x16, x17 = used by linker (avoid in long-lived allocations)
  - x0-x7 = argument/return (caller-saved, but available)
  - x19-x28 = callee-saved (must save/restore if used)
  Available for allocation: ~25 registers (x0-x17 minus x18, plus x19-x28)

- **FPR**: d0-d31 (or s0-s31 for single-precision). 32 registers:
  - d0-d7 = argument/return (caller-saved)
  - d8-d15 = callee-saved
  - d16-d31 = caller-saved
  Available for allocation: all 32

### 4. Callee-Saved Register Handling
If the allocator assigns a callee-saved register (x19-x28, d8-d15), the function prologue must save it and the epilogue must restore it:

```asm
; If we use x19, x20, d8:
_func:
    stp x29, x30, [sp, #-48]!
    stp x19, x20, [sp, #16]
    str d8, [sp, #32]
    mov x29, sp
    ; ... function body uses x19, x20, d8 ...
    ldr d8, [sp, #32]
    ldp x19, x20, [sp, #16]
    ldp x29, x30, [sp], #48
    ret
```

The save/restore set is determined after register allocation — we only save registers we actually used.

### 5. Spill Code Generation
When a virtual register is spilled:
- Allocate a stack frame slot for it
- Before each use: insert `ldr` from the spill slot
- After each definition: insert `str` to the spill slot

Optimize: if a spilled value is used multiple times in a basic block, reload once at block entry and keep it in a scratch register.

### 6. Move Insertion & Coalescing
When an IR operation requires specific registers (e.g., function arguments in x0-x7), insert moves:
```asm
    mov x0, x19         ; move value to argument register
    bl _some_func
    mov x20, x0         ; move return value to allocated register
```

Basic coalescing: if the source and destination of a move are allocated to the same register, eliminate the move.

### 7. Register Hints
The allocator should prefer:
- Argument values in their natural registers (x0-x7) to avoid moves
- Return values in x0/d0 to avoid a move before `ret`
- Frequently used values in callee-saved registers (avoid reload after calls)

## Testing Strategy

### Correctness First
Every program that worked with the naive allocator must still produce identical output. This is the primary invariant.

### Register Pressure Tests
Programs with many live variables that force spilling:
```fortran
subroutine many_vars()
    integer :: a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p
    ! operations using all 16 variables
    ! must spill some
end subroutine
```

### Callee-Save Tests
Call a function, verify the caller's values are preserved:
```fortran
x = 42
call heavy_function()     ! uses many registers internally
print *, x                ! must still be 42
```

### Calling Convention Integration
Verify arguments land in correct registers, return values captured correctly, no register conflicts across calls.

### Performance Comparison
Compile a compute-heavy program with naive allocation vs. linear scan. Measure:
- Code size (fewer spill instructions = smaller)
- Runtime (fewer memory accesses = faster)

Not a gate for this sprint, but a sanity check.

## Definition of Done
- Linear scan register allocator implemented
- Liveness analysis correct
- Callee-saved registers properly saved/restored
- Spill code generated when registers exhausted
- All previously passing tests still pass (correctness preserved)
- Programs with high register pressure compile and run correctly
- No use of x18 (Apple platform reserved)
- Frame pointer always maintained (x29, Apple requirement)
- Move coalescing eliminates unnecessary register-to-register moves
- `cargo test` register allocation tests pass
