# Sprint 16: IR — Complex Lowering (Arrays, Strings, Control Flow)

## Prerequisites
Sprint 15 (IR design, basic lowering)

## Goals
Extend IR lowering to handle Fortran's complex features: array descriptors, allocatable variables, character strings, control flow, and subprogram calls. After this sprint, the full fortsh AST can be lowered to IR.

## Deliverables

### 1. Array Descriptor Lowering
Fortran arrays carry metadata (bounds, strides) in descriptors:

```rust
// Array descriptor layout (in IR):
struct ArrayDescriptor {
    base_addr: *mut T,        // pointer to data
    elem_size: i64,           // size of one element
    rank: i32,                // number of dimensions
    allocated: bool,          // is this allocatable and currently allocated?
    dims: [DimInfo; MAX_RANK],
}

struct DimInfo {
    lower_bound: i64,
    upper_bound: i64,
    stride: i64,              // in elements (for non-contiguous sections)
}
```

Lowering patterns:
```fortran
real :: a(10, 20)          ! → alloca descriptor, set bounds, alloca 200*f32
a(i, j) = 5.0             ! → compute offset from descriptor, store
x = a(3, 4)               ! → compute offset, load
call sub(a(:, 1))          ! → create section descriptor (stride != 1)
```

Array element address calculation:
```
addr = base + ((i - lower1) * stride1 + (j - lower2) * stride2) * elem_size
```

### 2. Allocatable Lowering
```fortran
real, allocatable :: a(:,:)
allocate(a(m, n))        ! → runtime call: malloc(m*n*sizeof(real)), set descriptor
a(i, j) = 1.0            ! → same as fixed array, but through descriptor
deallocate(a)             ! → runtime call: free(base_addr), clear descriptor
```

Scope exit: if `a` is still allocated when the scope ends, automatically deallocate.

Allocatable assignment (F2003):
```fortran
a = b    ! if shapes differ, reallocate a to match b, then copy
```
This lowers to: check shapes, if different then deallocate+reallocate, memcpy.

### 3. Character String Lowering
Characters are {pointer, length} pairs:
```fortran
character(10) :: fixed_str       ! → alloca 10 bytes, len=10
character(:), allocatable :: s   ! → descriptor with deferred length
s = 'hello'                      ! → allocate 5 bytes, copy, set len=5
s = s // ' world'               ! → allocate 11 bytes, concat, free old, set len=11
```

**This is where gfortran dies on ARM64.** Our lowering must be meticulous:
- Always track length alongside data pointer
- Reallocation for deferred-length assignment must be atomic (allocate new, copy, free old)
- Substring operations create descriptors pointing into the original string (no copy)
- Trim returns a new descriptor with adjusted length (no trailing spaces)

### 4. Control Flow Lowering
```fortran
! IF → conditional branch
if (x > 0) then      ! → cond_branch %cmp, bb_then, bb_else
    y = sqrt(x)       !    bb_then: ... branch bb_end
else                   !    bb_else: ...
    y = 0.0           !              branch bb_end
end if                 !    bb_end:  (merge point)

! DO loop → loop with back-edge
do i = 1, n           ! → bb_init: i=1; branch bb_check
    a(i) = 0.0        !    bb_check: cmp i<=n; cond_branch bb_body, bb_exit
end do                 !    bb_body: ...; i=i+1; branch bb_check
                       !    bb_exit: ...

! SELECT CASE → switch terminator
select case (x)       ! → switch %x, [1→bb1, 2→bb2, ...], bb_default
case (1)
    ...
case (2)
    ...
case default
    ...
end select
```

### 5. Subprogram Call Lowering
```fortran
call sub(x, y, z)
! Lowers to:
! 1. Evaluate arguments
! 2. For non-VALUE arguments, pass address (Fortran is pass-by-reference by default)
! 3. For VALUE arguments, pass value
! 4. For array arguments, pass descriptor
! 5. For character arguments, pass {ptr, len} — hidden length argument
! 6. Emit call instruction

result = func(a, b)
! Same as above but captures return value
```

**Hidden arguments** — Fortran calling conventions pass extra "hidden" arguments:
- Character length for each character dummy argument
- Array descriptors for assumed-shape arrays
- These are appended after the explicit arguments

### 6. Module Variable Access
Module variables are globals in the IR:
```fortran
module config
    integer :: debug_level = 0
end module

subroutine foo()
    use config
    debug_level = 3    ! → store to global @config::debug_level
end subroutine
```

### 7. Implicit Deallocation
At every scope exit, deallocate all local allocatable variables:
```fortran
subroutine foo()
    real, allocatable :: temp(:)
    allocate(temp(100))
    ! ... use temp ...
    ! at end: automatically deallocate temp
end subroutine
```

This includes early returns and branches out of scope. The IR must insert deallocation calls on all exit paths.

## Testing Strategy

### Array Tests
- Fixed-size array allocation and access
- Multi-dimensional array element address computation
- Array section descriptor creation
- Assumed-shape argument passing

### Allocatable Tests
- Allocate, use, deallocate cycle
- Automatic reallocation on assignment
- Automatic deallocation at scope exit
- Allocatable character assignment (the critical test)

### String Tests
- Fixed-length character assignment (pad with spaces)
- Deferred-length character assignment (reallocate)
- Concatenation producing new string
- Substring reference (no copy, just descriptor adjustment)
- Compare IR output against expected patterns

### Control Flow Tests
- If/else → correct branch structure
- DO loop → correct loop structure with back edge
- Nested loops → correct nesting
- EXIT/CYCLE → correct branch targets
- SELECT CASE → switch with correct dispatch

### Call Tests
- Simple scalar arguments (pass by reference)
- VALUE arguments (pass by value)
- Array arguments (pass descriptor)
- Character arguments (hidden length)
- Optional arguments (null pointer when absent)

### IR Verification
Run the IR verifier after every lowering test. All generated IR must pass.

## Definition of Done
- Array descriptor operations lower correctly
- Allocatable allocate/deallocate/reassign lower correctly
- Character string operations lower correctly (including deferred-length)
- All control flow constructs lower to correct basic block structure
- Subprogram calls lower with correct argument passing
- Module variables lower to globals
- Implicit deallocation at scope exits
- IR verifier passes on all generated IR
- Complex Fortran programs produce valid IR
- `cargo test` all lowering tests pass
