# Sprint 20: Codegen — Functions & Calling Convention (AAPCS64)

## Prerequisites
Sprint 19 (control flow codegen)

## Goals
Implement the ARM64 calling convention (Apple's AAPCS64 variant) for Fortran subprogram calls. This covers argument passing, return values, stack frame interplay between caller and callee, and Fortran-specific calling patterns (pass-by-reference, hidden length arguments).

## Deliverables

### 1. Apple AAPCS64 Basics
**Integer/pointer arguments**: x0-x7 (first 8), then stack
**Floating-point arguments**: d0-d7 (first 8), then stack
**Return value**: x0 (integer/pointer), d0 (float), or memory (for large structs)
**Callee-saved**: x19-x28, d8-d15
**Caller-saved**: x0-x18, d0-d7, d16-d31
**Frame pointer**: x29 (always, on Apple platforms — frame pointer elision is not allowed)
**Link register**: x30
**Stack pointer**: sp (16-byte aligned always)

### 2. Fortran Argument Passing
Fortran is **pass-by-reference** by default. Each argument (except `VALUE`) is passed as a pointer:

```fortran
subroutine add(a, b, result)
    real, intent(in) :: a, b
    real, intent(out) :: result
    result = a + b
end subroutine
```

Calling convention:
- x0 = pointer to `a`
- x1 = pointer to `b`
- x2 = pointer to `result`

At the call site:
```asm
    ; caller
    add x0, sp, #a_offset     ; address of a
    add x1, sp, #b_offset     ; address of b
    add x2, sp, #result_offset ; address of result
    bl _add
```

At the callee:
```asm
    ; callee
_add:
    stp x29, x30, [sp, #-32]!
    mov x29, sp
    str x0, [sp, #16]          ; save arg pointers
    str x1, [sp, #20]
    str x2, [sp, #24]
    ; load values through pointers
    ldr x8, [x0]               ; a
    ldr x9, [x1]               ; b
    ; ...compute...
    str result, [x2]           ; store to result
    ldp x29, x30, [sp], #32
    ret
```

### 3. VALUE Attribute
`VALUE` arguments are passed by value (like C):
```fortran
subroutine foo(x) bind(c)
    integer, value :: x    ! passed in x0 directly, not as pointer
end subroutine
```

### 4. Function Return Values
```fortran
function square(x) result(y)
    real, intent(in) :: x
    real :: y
    y = x * x
end function
```

- Scalar return: in x0 (integer) or d0 (float)
- Character return: hidden first argument (pointer to result buffer + length)
- Array return: hidden first argument (pointer to result descriptor)

### 5. Hidden Arguments
Fortran passes hidden arguments for character and array dummies:

```fortran
subroutine process(name, data, n)
    character(*), intent(in) :: name
    real, intent(in) :: data(:)
    integer, intent(in) :: n
end subroutine
```

Actual calling convention:
```
x0 = pointer to name data
x1 = pointer to data descriptor
x2 = pointer to n
x3 = length of name (hidden character length argument)
```

Character lengths are passed as `i64` after all explicit arguments.

### 6. Optional Arguments
```fortran
subroutine foo(x, y)
    real, intent(in) :: x
    real, intent(in), optional :: y
end subroutine

call foo(1.0)           ! y absent → pass null pointer
call foo(1.0, 2.0)      ! y present → pass address
```

At the callee, `present(y)` checks if the pointer is null:
```asm
    ldr x8, [sp, #y_arg_offset]
    cmp x8, #0
    cset x9, ne          ; x9 = 1 if present, 0 if absent
```

### 7. Recursive Functions
Stack frame must support recursion — each invocation gets its own frame:
```fortran
recursive function factorial(n) result(f)
    integer, intent(in) :: n
    integer :: f
    if (n <= 1) then
        f = 1
    else
        f = n * factorial(n - 1)
    end if
end function
```

This "just works" with proper frame setup, but we must ensure the frame pointer chain is correct for debugging.

### 8. Internal Subprograms (Host Association)
Internal subprograms access the host's variables. Implementation: pass a "static link" (pointer to host's frame) as a hidden argument:

```fortran
subroutine outer()
    integer :: x = 5
contains
    subroutine inner()
        print *, x          ! accesses outer's x via static link
    end subroutine
end subroutine
```

## Testing Strategy

### Basic Call Tests
```fortran
! Subroutine call, scalar arguments
call swap(a, b)
! Function call, return value
y = square(x)
! Multiple return types
i = int_func()
x = real_func()
```

### Recursion Tests
- Factorial (integer recursion)
- Fibonacci (double recursion)
- Verify correct results for edge cases (n=0, n=1, n=20)

### Character Argument Tests
- Pass character string to subroutine
- Verify hidden length argument is correct
- Character function return

### Optional Argument Tests
- Call with optional present
- Call with optional absent
- `present()` intrinsic returns correct value

### Internal Subprogram Tests
- Internal sub reads host variable
- Internal sub modifies host variable
- Nested internal subprograms (three levels)

### ABI Compatibility Tests
Write a C function, call it from Fortran (via `bind(c)`). Write a Fortran function with `bind(c)`, call it from C. Verify interop works — this tests that our calling convention matches clang's.

## Definition of Done
- Subroutine and function calls work with scalar arguments
- Pass-by-reference is default, VALUE passes by value
- Character hidden length arguments passed correctly
- Optional arguments (null pointer for absent) work
- `present()` intrinsic works
- Function return values work (integer, real, character)
- Recursive functions work correctly
- Internal subprograms access host variables
- ABI compatible with clang-compiled C code (verified with interop test)
- `cargo test` calling convention tests pass
