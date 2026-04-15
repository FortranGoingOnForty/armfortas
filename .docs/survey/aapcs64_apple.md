# Apple AAPCS64 Calling Convention

Reference: ARM Procedure Call Standard for AArch64 (AAPCS64), with Apple platform deviations.

## Register Usage

### General Purpose Registers
| Register | Role | Saved by |
|----------|------|----------|
| X0-X7 | Arguments / return values | Caller |
| X8 | Indirect result location (struct return) | Caller |
| X9-X15 | Temporary / scratch | Caller |
| X16 (IP0) | Intra-procedure-call scratch / PLT | Caller |
| X17 (IP1) | Intra-procedure-call scratch / PLT | Caller |
| **X18** | **RESERVED on Apple platforms — DO NOT USE** | — |
| X19-X28 | Callee-saved | Callee |
| X29 (FP) | Frame pointer (**mandatory on Apple**) | Callee |
| X30 (LR) | Link register (return address) | Callee |
| SP | Stack pointer (16-byte aligned always) | — |

### Floating Point / SIMD Registers
| Register | Role | Saved by |
|----------|------|----------|
| D0-D7 / S0-S7 | Arguments / return values | Caller |
| D8-D15 / S8-S15 | Callee-saved (lower 64 bits only!) | Callee |
| D16-D31 | Temporary / scratch | Caller |

**Important**: Only the lower 64 bits of Q8-Q15 are callee-saved. The upper 64 bits (the SIMD portion) are caller-saved.

## Argument Passing

### Integer / Pointer Arguments
- First 8 in X0-X7
- Remaining on stack (8-byte slots, naturally aligned)
- Smaller types (i8, i16, i32) are **zero-extended** or **sign-extended** to fill the register

### Floating-Point Arguments
- First 8 in D0-D7 (double) or S0-S7 (float)
- Remaining on stack

### Mixed Arguments
Integer and FP argument counters are **independent**:
```c
void foo(int a, double b, int c, double d);
// a → X0, b → D0, c → X1, d → D1
```

### Small Structs (≤ 16 bytes)
- Passed in 1-2 registers (X or D depending on content)
- Homogeneous float aggregates (HFAs) up to 4 floats/doubles → in D registers

### Large Structs (> 16 bytes)
- Caller allocates memory, passes pointer in next available X register
- For return values: caller passes destination pointer in X8

## Return Values
- Integer/pointer: X0 (and X1 for 128-bit)
- Float/double: D0 (and D1 for complex)
- Small struct: X0/X1 or D0-D3
- Large struct: caller provides buffer via X8

## Stack Frame Layout

Apple **requires frame pointer** (X29). Frame pointer elision is not allowed on Apple platforms (unlike Linux ARM64 where it's optional).

```
High addresses
┌────────────────────┐
│ Caller's frame      │
├────────────────────┤  ← Previous SP (caller's SP)
│ Arguments on stack  │  (if more than 8 int + 8 float args)
├────────────────────┤
│ Saved LR (X30)      │  } always saved as pair
│ Saved FP (X29)      │  } via STP X29, X30, [SP, #-N]!
├────────────────────┤  ← FP (X29) points here
│ Saved callee-saved  │  X19-X28, D8-D15 (only those used)
│ registers           │
├────────────────────┤
│ Local variables     │
│ Spill slots         │
├────────────────────┤
│ Outgoing arguments  │  (for calls this function makes)
├────────────────────┤  ← SP (always 16-byte aligned)
Low addresses
```

### Function Prologue (typical)
```asm
; Save FP and LR, allocate frame
stp x29, x30, [sp, #-FRAME_SIZE]!
mov x29, sp

; Save callee-saved registers (if used)
stp x19, x20, [sp, #16]
stp x21, x22, [sp, #32]
str d8, [sp, #48]
```

### Function Epilogue (typical)
```asm
; Restore callee-saved registers
ldr d8, [sp, #48]
ldp x21, x22, [sp, #32]
ldp x19, x20, [sp, #16]

; Restore FP/LR and deallocate frame
ldp x29, x30, [sp], #FRAME_SIZE
ret
```

## Apple-Specific Deviations from Standard AAPCS64

1. **X18 is reserved**: Used by the OS for thread-local storage / platform purposes. Never touch it.

2. **Frame pointer is mandatory**: `-fomit-frame-pointer` is ignored on Apple ARM64. X29 must always be a valid frame pointer for each function.

3. **Stack must be 16-byte aligned at all times**: Not just at function calls — at *every* instruction. This is stricter than some ARM64 Linux implementations.

4. **Red zone**: There is NO red zone on Apple ARM64 (unlike x86-64 macOS which has 128 bytes). Never access below SP.

5. **BTI (Branch Target Identification)**: Not currently enforced on macOS but may be in the future. Consider emitting BTI instructions at branch targets.

6. **PAC (Pointer Authentication)**: Used in system frameworks. We don't need to emit PAC instructions for user code, but we should be aware that system libraries use them.

## Fortran-Specific Calling Convention Notes

### Pass-by-Reference (Default)
Fortran passes arguments by reference by default. Each argument becomes a pointer:
```fortran
subroutine add(a, b, result)   ! three pointer arguments
    real, intent(in) :: a, b
    real, intent(out) :: result
```
→ X0 = &a, X1 = &b, X2 = &result

### VALUE Attribute
```fortran
subroutine foo(x) bind(c)
    integer, value :: x        ! passed in X0 directly, not as pointer
```

### Hidden Character Length
Character arguments carry a hidden length parameter after all explicit arguments:
```fortran
subroutine print_name(name)
    character(*), intent(in) :: name
```
→ X0 = pointer to character data, X1 = length (i64)

If there are multiple character arguments, all lengths are appended in order after the explicit arguments.

### Array Descriptors
Assumed-shape array arguments pass a descriptor pointer:
```fortran
subroutine process(array)
    real, intent(in) :: array(:,:)
```
→ X0 = pointer to ArrayDescriptor struct (base_addr, rank, dims[], etc.)

### Struct Return for Complex Types
Functions returning derived types or arrays use X8 for the return buffer:
```fortran
function make_pair(a, b) result(p)
    type(pair) :: p     ! if sizeof(pair) > 16, returned via X8
```

## macOS Syscall Convention
On Apple ARM64, syscalls use:
- X16 = syscall number
- X0-X5 = arguments
- SVC #0x80 = invoke syscall
- X0 = return value (negative on error)

Key syscalls: write(4), exit(1), open(5), close(6), mmap(197), munmap(73)
