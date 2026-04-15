# Sprint 14: Semantic Analysis — Advanced Validation

## Prerequisites
Sprint 13 (type system)

## Goals
Complete semantic analysis with validation rules that go beyond type checking: allocatable/pointer semantics, intent enforcement, pure/elemental constraints, defined operator resolution, and other standard conformance checks. After this sprint, the frontend is complete.

## Deliverables

### 1. Allocatable & Pointer Validation
```fortran
! Allocatable rules:
real, allocatable :: a(:)
allocate(a(100))           ! ok
a = [1.0, 2.0, 3.0]       ! ok: automatic reallocation (F2003)
a = b                      ! ok if b is conformable array
deallocate(a)              ! ok

! Pointer rules:
real, target :: x
real, pointer :: p
p => x                     ! ok: pointer assignment
p => null()                ! ok
p = 3.14                   ! ok: assigns through pointer
allocate(p)                ! ok: allocate pointed-to memory
```

Checks:
- Only allocatable/pointer variables can appear in ALLOCATE/DEALLOCATE
- Pointer assignment (`=>`) only to pointer on left
- Target of pointer assignment must have TARGET attribute or be a pointer
- Allocatable assignment follows shape conformance rules
- Deferred-length character must be allocatable or pointer

### 2. Intent Enforcement
```fortran
subroutine foo(x, y, z)
    real, intent(in) :: x       ! read-only
    real, intent(out) :: y      ! write-only, undefined on entry
    real, intent(inout) :: z    ! read-write
    
    x = 1.0       ! ERROR: can't modify intent(in)
    print *, y     ! WARNING: y is undefined (intent(out))
    z = x + z      ! ok
end subroutine
```

At call sites:
- Can't pass a literal or parameter to `intent(out)` or `intent(inout)`
- Can't pass a section of a non-contiguous array to a contiguous dummy (unless copy-in/copy-out)
- `intent(out)` allocatable dummy gets deallocated on entry

### 3. Pure & Elemental Constraints
```fortran
pure function square(x) result(y)
    real, intent(in) :: x
    real :: y
    y = x * x
    ! Cannot: modify global variables, do I/O, call impure procedures,
    !         use STOP/ERROR STOP, have SAVE variables
end function

elemental function clamp(x, lo, hi) result(y)
    real, intent(in) :: x, lo, hi
    real :: y
    y = max(lo, min(x, hi))
    ! All above pure constraints, plus:
    ! All arguments must be scalar
    ! Can be called with arrays (applied element-wise)
end function
```

Validate all pure/elemental constraints.

### 4. Defined Operator Validation
```fortran
interface operator(+)
    function add_vec(a, b) result(c)
        type(vector), intent(in) :: a, b
        type(vector) :: c
    end function
end interface
```

Checks:
- Operator function has correct number of arguments (1 for unary, 2 for binary)
- Arguments are `intent(in)`
- Function (not subroutine)
- Matches when used in expressions

### 5. Type-Bound Procedure Validation
```fortran
type :: shape
contains
    procedure :: area                    ! specific type-bound
    procedure :: draw => draw_shape      ! renamed
    procedure, pass(self) :: compare     ! explicit PASS
    procedure, nopass :: factory         ! no passed-object
    generic :: operator(==) => compare   ! operator overload
    procedure, deferred :: volume        ! abstract — must be overridden
end type
```

Validate:
- Referenced procedures exist
- PASS/NOPASS consistency
- Deferred procedures only in abstract types
- Overriding procedures match interface of parent
- Non-overridable (NON_OVERRIDABLE attribute) respected

### 6. ASSOCIATE & SELECT TYPE Validation
```fortran
associate (n => size(array), val => array(i)%component)
    ! n and val are valid names within this block
    ! n has type/shape of size(array)
end associate

select type (x => polymorphic_var)
type is (integer)
    ! x is integer here
type is (real)
    ! x is real here
class is (base_type)
    ! x is CLASS(base_type) here
class default
    ! x is CLASS(*) here
end select
```

### 7. Statement Label Validation
- All GOTO targets exist
- Labeled DO loop structure is well-formed
- FORMAT labels referenced by I/O statements exist
- No duplicate labels in a scope
- Arithmetic IF labels exist

### 8. Standard Conformance (--std= Enforcement)
Based on the selected standard:
- Warn/error on features not in the selected standard
- Track which standard introduced each feature
- Provide diagnostic: "error: DO CONCURRENT requires --std=f2008 or later"

## Testing Strategy

### Constraint Violation Tests
Write Fortran programs that violate each constraint and verify the correct error is produced:
- Modify `intent(in)` argument → error
- I/O in pure function → error
- Non-allocatable in ALLOCATE → error
- Missing non-optional argument → error

### Valid Code Tests
Ensure all these checks don't produce false positives on correct code. fortsh source is the primary test case.

### Standard Mode Tests
Compile the same code with different `--std=` settings:
```fortran
do concurrent (i = 1:n)   ! ok with --std=f2008, error with --std=f95
```

### Error Message Quality Tests
Verify error messages include:
- Source location (file:line:column)
- Clear description of the problem
- Suggestion for fix where applicable

## Definition of Done
- Allocatable/pointer semantics validated
- Intent enforcement complete
- Pure/elemental constraints checked
- Defined operator validation
- Type-bound procedure validation
- ASSOCIATE/SELECT TYPE semantics correct
- Label validation (GOTO, FORMAT, DO)
- --std= conformance checking (at least for F2018 vs earlier)
- Clear, helpful error messages for all violations
- Zero false positives on fortsh source
- **Frontend complete**: source → preprocess → lex → parse → sema produces a fully typed, validated AST
- `cargo test` all semantic analysis tests pass
