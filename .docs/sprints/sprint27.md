# Sprint 27: iso_c_binding

## Prerequisites
Sprint 20 (calling convention), Sprint 22 (memory), Sprint 23 (strings)

## Goals
Implement the `iso_c_binding` intrinsic module — Fortran's standard interface for interoperating with C. This is critical for fortsh, which uses C interop extensively (3 C files for string ops, fd wrappers, and terminal size). Our implementation must produce code that's ABI-compatible with clang-compiled C on Apple ARM64.

## Deliverables

### 1. Named Constants
The `iso_c_binding` module provides kind parameters that match C types:

```fortran
use iso_c_binding
! These must map to actual C type sizes on ARM64 macOS:
integer(c_int)          ! 4 bytes (int)
integer(c_long)         ! 8 bytes (long — 64-bit on macOS!)
integer(c_long_long)    ! 8 bytes (long long)
integer(c_short)        ! 2 bytes (short)
integer(c_int8_t)       ! 1 byte
integer(c_int16_t)      ! 2 bytes
integer(c_int32_t)      ! 4 bytes
integer(c_int64_t)      ! 8 bytes
integer(c_size_t)       ! 8 bytes (size_t)
integer(c_intptr_t)     ! 8 bytes (intptr_t)
integer(c_ptrdiff_t)    ! 8 bytes (ptrdiff_t)
real(c_float)           ! 4 bytes (float)
real(c_double)          ! 8 bytes (double)
character(c_char)       ! 1 byte (char)
! Special:
integer(c_signed_char)  ! 1 byte
logical(c_bool)         ! 1 byte (_Bool)
type(c_ptr)             ! opaque pointer (8 bytes)
type(c_funptr)          ! function pointer (8 bytes)
```

**Important**: `c_long` is 8 bytes on macOS ARM64 (LP64 model) but 4 bytes on Windows. We must use the correct value for our target.

### 2. BIND(C) Subprograms
```fortran
subroutine my_func(x, n) bind(c, name='my_func')
    real(c_double), value :: x
    integer(c_int), value :: n
end subroutine
```

BIND(C) means:
- The function uses the C calling convention (pass by value for VALUE args)
- The function is emitted with the specified name (no Fortran name mangling)
- No hidden arguments (no character lengths, no descriptors)
- Arguments with VALUE attribute are passed by value (in registers)
- Arguments without VALUE are passed by reference (pointer)

### 3. BIND(C) Derived Types
```fortran
type, bind(c) :: point
    real(c_double) :: x, y, z
end type

type, bind(c) :: stat_buf
    integer(c_long) :: st_dev
    integer(c_long) :: st_ino
    ! ... etc
end type
```

BIND(C) types must match C struct layout exactly:
- Same member order
- Same alignment (Apple ARM64: natural alignment)
- Same padding
- No Fortran descriptor overhead

**Apple ARM64 alignment rules:**
- `char`: 1-byte aligned
- `short`: 2-byte aligned
- `int`, `float`: 4-byte aligned
- `long`, `double`, pointers: 8-byte aligned
- Struct: aligned to its most-aligned member
- Struct size: padded to multiple of alignment

### 4. C_LOC, C_FUNLOC, C_F_POINTER, C_F_PROCPOINTER
```fortran
! Get C address of Fortran variable
type(c_ptr) :: p
real, target :: x
p = c_loc(x)

! Get C function pointer
type(c_funptr) :: fp
fp = c_funloc(my_function)

! Convert C pointer back to Fortran pointer
real, pointer :: fptr
call c_f_pointer(p, fptr)

! Convert C pointer to Fortran array pointer (with shape)
real, pointer :: arr(:)
call c_f_pointer(p, arr, [100])  ! 100-element array
```

### 5. C_ASSOCIATED
```fortran
if (c_associated(p)) then          ! is p non-null?
if (c_associated(p, q)) then       ! do p and q point to same address?
```

### 6. C_SIZEOF (F2008)
```fortran
n = c_sizeof(x)     ! size in bytes of x's C representation
```

### 7. C String Handling
```fortran
character(c_char), dimension(*) :: c_str    ! C string (null-terminated)

! Common pattern in fortsh:
interface
    integer(c_int) function strlen(s) bind(c)
        import :: c_char, c_int
        character(c_char), intent(in) :: s(*)
    end function
end interface
```

Fortran-to-C string conversion: append `C_NULL_CHAR`. C-to-Fortran: read until null, create Fortran character.

### 8. The iso_c_binding Module Implementation
This is a "built-in module" — not compiled from source but constructed internally by the compiler:

```rust
fn create_iso_c_binding_module() -> Module {
    let mut m = Module::new("iso_c_binding");
    
    // Add all named constants
    m.add_parameter("c_int", Type::Integer(4), Value::Int(4));
    m.add_parameter("c_long", Type::Integer(8), Value::Int(8));  // 8 on macOS ARM64
    // ... etc
    
    // Add derived types
    m.add_type("c_ptr", make_c_ptr_type());
    m.add_type("c_funptr", make_c_funptr_type());
    
    // Add procedures
    m.add_procedure("c_loc", ...);
    m.add_procedure("c_funloc", ...);
    m.add_procedure("c_f_pointer", ...);
    m.add_procedure("c_associated", ...);
    m.add_procedure("c_sizeof", ...);
    
    // Add constants
    m.add_parameter("c_null_ptr", ...);
    m.add_parameter("c_null_funptr", ...);
    m.add_parameter("c_null_char", ...);
    m.add_parameter("c_new_line", ...);
    m.add_parameter("c_carriage_return", ...);
    m.add_parameter("c_horizontal_tab", ...);
    m.add_parameter("c_vertical_tab", ...);
    m.add_parameter("c_backspace", ...);
    m.add_parameter("c_alert", ...);
    m.add_parameter("c_form_feed", ...);
    
    m
}
```

## Testing Strategy

### ABI Compatibility Tests (The Critical Tests)
Write C functions with clang, call them from Fortran compiled by `afs`:

```c
// test_interop.c (compiled with clang)
#include <stdio.h>
int add_ints(int a, int b) { return a + b; }
double compute(double x, int n) { return x * n; }
void fill_array(double *arr, int n) { for(int i=0;i<n;i++) arr[i] = i; }

struct Point { double x, y, z; };
double point_distance(struct Point *p) {
    return sqrt(p->x*p->x + p->y*p->y + p->z*p->z);
}
```

```fortran
! test_interop.f90 (compiled with afs)
program test
    use iso_c_binding
    interface
        integer(c_int) function add_ints(a, b) bind(c)
            import c_int
            integer(c_int), value :: a, b
        end function
    end interface
    print *, add_ints(3, 4)  ! must print 7
end program
```

Compile both, link together, run. This is the definitive test.

### Struct Layout Tests
Create BIND(C) derived types, verify `c_sizeof` matches `sizeof` in C. Verify member offsets match.

### C Pointer Tests
- `c_loc` on target variable, pass to C function, verify address
- `c_f_pointer` back to Fortran, verify value
- `c_associated` null check
- `c_f_pointer` with shape for arrays

### fortsh Interop Test
Compile fortsh's C interop modules against our iso_c_binding:
- Parse the interface blocks in fortsh's C interop modules
- Verify they type-check correctly
- Eventually: link with fortsh's C files and verify they work

## Key Technical Notes

### Apple ARM64 vs Linux ARM64 ABI Differences
- `c_long` = 8 bytes on both (LP64), but this differs on some targets
- Stack alignment: Apple requires 16-byte always; Linux is more relaxed
- Variadic functions: Apple passes float args in FP registers; some Linux ABIs differ
- `_Bool` / `c_bool`: 1 byte on Apple, stored as 0 or 1

### No Hidden Arguments
BIND(C) functions have NO hidden arguments — no character lengths, no array descriptors. Character arguments are passed as `char *` (pointer only, length is not passed). The Fortran code must handle length tracking itself (typically using C_NULL_CHAR as terminator).

## Definition of Done
- All `iso_c_binding` named constants have correct values for ARM64 macOS
- BIND(C) subprograms use C calling convention
- BIND(C) derived types match C struct layout exactly
- C_LOC, C_FUNLOC, C_F_POINTER, C_ASSOCIATED work
- Can call clang-compiled C functions from afs-compiled Fortran
- Can call afs-compiled BIND(C) functions from clang-compiled C
- Struct member offsets match between Fortran and C
- fortsh's iso_c_binding interface blocks type-check correctly
- `cargo test` C interop tests pass
