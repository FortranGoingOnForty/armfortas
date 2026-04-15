# Sprint 19: Codegen — Control Flow & Loops

## Prerequisites
Sprint 18 (hello world works)

## Goals
Extend codegen to handle all control flow: conditional branches, loops (DO, DO WHILE, infinite DO), SELECT CASE, and EXIT/CYCLE. After this sprint, programs with interesting logic compile correctly.

## Deliverables

### 1. Conditional Branches
```fortran
if (x > 0) then
    y = 1
else if (x < 0) then
    y = -1
else
    y = 0
end if
```

ARM64 codegen:
```asm
    ldr x8, [sp, #x_offset]       ; load x
    cmp x8, #0
    b.le .Lelse_if                 ; not greater → else_if
    ; then block
    mov x8, #1
    str x8, [sp, #y_offset]
    b .Lend_if
.Lelse_if:
    cmp x8, #0
    b.ge .Lelse                    ; not less → else
    ; else_if block
    mov x8, #-1
    str x8, [sp, #y_offset]
    b .Lend_if
.Lelse:
    ; else block
    str xzr, [sp, #y_offset]
.Lend_if:
```

### 2. Counted DO Loops
```fortran
do i = 1, n
    a(i) = real(i)
end do
```

ARM64 pattern:
```asm
    mov x8, #1                    ; i = 1
    str x8, [sp, #i_offset]
    ldr x9, [sp, #n_offset]       ; load n
.Ldo_check:
    ldr x8, [sp, #i_offset]
    cmp x8, x9
    b.gt .Ldo_exit                ; i > n → exit
    ; loop body
    ; ...
    ; increment
    ldr x8, [sp, #i_offset]
    add x8, x8, #1
    str x8, [sp, #i_offset]
    b .Ldo_check
.Ldo_exit:
```

**Loop with step:**
```fortran
do i = 10, 1, -1       ! step = -1
```
When step is negative, the termination test flips: `i < end` instead of `i > end`.

**General step handling**: Fortran allows non-unit steps including runtime-computed steps. The iteration count is computed once before the loop:
```
trip_count = max(0, (end - start + step) / step)
```

### 3. DO WHILE
```fortran
do while (x > epsilon)
    x = x / 2.0
end do
```

Standard while-loop pattern: test at top, branch back from bottom.

### 4. Infinite DO with EXIT
```fortran
do
    call compute(x)
    if (converged(x)) exit
end do
```

Unconditional branch at bottom, EXIT jumps to after the loop.

### 5. EXIT and CYCLE
```fortran
outer: do i = 1, n
    inner: do j = 1, m
        if (a(i,j) < 0) exit outer    ! breaks both loops
        if (a(i,j) == 0) cycle inner   ! skips to next j
        call process(a(i,j))
    end do inner
end do outer
```

EXIT jumps to the block after the named construct. CYCLE jumps to the increment/test of the named construct. Both require tracking the label→block mapping for named constructs through nested scopes.

### 6. SELECT CASE
```fortran
select case (command)
case ('quit', 'exit')
    running = .false.
case ('help')
    call show_help()
case ('version')
    call show_version()
case default
    call execute_command(command)
end select
```

**Integer select case** → ARM64 jump table (if cases are dense) or binary search/comparison chain (if sparse).

**Character select case** → comparison chain with string compare runtime calls.

**Range cases** (`case (1:10)`) → range check.

### 7. Short-Circuit Evaluation
Fortran does **not** guarantee short-circuit evaluation (unlike C). However, many real programs assume it:
```fortran
if (allocated(a) .and. size(a) > 0) then
```

We follow common practice: short-circuit `.and.` and `.or.` for safety, matching gfortran's behavior. Add a flag for strict standard conformance (evaluate both sides) if needed.

## Testing Strategy

### Correctness Tests
For each control flow pattern, compile a program that exercises it and verify output:
- If/else → print which branch was taken
- DO loop → print sum of loop variable
- DO WHILE → print iteration count
- Nested loops with EXIT → print coordinates where exit occurred
- SELECT CASE → print which case matched

### Trip Count Tests
```fortran
! These edge cases must work:
do i = 1, 0          ! zero iterations
do i = 1, 1          ! one iteration
do i = 10, 1, -1     ! reverse, 10 iterations
do i = 1, 10, 3      ! 1, 4, 7, 10 — 4 iterations
do i = 1, 10, 0      ! error: zero step
```

### FizzBuzz Test
The classic:
```fortran
program fizzbuzz
    integer :: i
    do i = 1, 100
        if (mod(i, 15) == 0) then
            print *, 'FizzBuzz'
        else if (mod(i, 3) == 0) then
            print *, 'Fizz'
        else if (mod(i, 5) == 0) then
            print *, 'Buzz'
        else
            print *, i
        end if
    end do
end program
```
Compile, run, verify all 100 lines of output.

## Definition of Done
- If/else-if/else chains compile and execute correctly
- All DO loop forms (counted, while, infinite, with step) work
- EXIT and CYCLE (including to named constructs) work
- SELECT CASE works for integer and character selectors
- Nested control flow to arbitrary depth works
- FizzBuzz compiles and produces correct output
- Trip count edge cases handled (zero iterations, negative step)
- `cargo test` control flow codegen tests pass
