# Sprint 10: Parser — Subprograms & Modules

## Prerequisites
Sprint 7-9 (expressions, declarations, control flow)

## Goals
Parse the organizational units of Fortran: programs, subroutines, functions, modules, and submodules. This is where Fortran's compilation model lives — modules define the interfaces between compilation units, and subprograms are the executable building blocks.

## Deliverables

### 1. AST Program Unit Nodes
```rust
enum ProgramUnit {
    Program {
        name: Option<String>,
        uses: Vec<UseStmt>,
        imports: Vec<ImportStmt>,
        implicit: Vec<Stmt>,
        decls: Vec<Decl>,
        body: Vec<Stmt>,
        contains: Vec<ProgramUnit>,  // internal subprograms
    },

    Module {
        name: String,
        uses: Vec<UseStmt>,
        imports: Vec<ImportStmt>,
        implicit: Vec<Stmt>,
        decls: Vec<Decl>,
        contains: Vec<ProgramUnit>,  // module subprograms
    },

    Submodule {
        parent: String,
        ancestor: Option<String>,
        name: String,
        uses: Vec<UseStmt>,
        decls: Vec<Decl>,
        contains: Vec<ProgramUnit>,
    },

    Subroutine {
        name: String,
        args: Vec<DummyArg>,
        bind: Option<BindSpec>,
        prefix: Vec<Prefix>,        // pure, elemental, recursive, etc.
        uses: Vec<UseStmt>,
        imports: Vec<ImportStmt>,
        implicit: Vec<Stmt>,
        decls: Vec<Decl>,
        body: Vec<Stmt>,
        contains: Vec<ProgramUnit>,
    },

    Function {
        name: String,
        args: Vec<DummyArg>,
        result: Option<String>,      // RESULT(name) clause
        return_type: Option<TypeSpec>,
        bind: Option<BindSpec>,
        prefix: Vec<Prefix>,
        uses: Vec<UseStmt>,
        imports: Vec<ImportStmt>,
        implicit: Vec<Stmt>,
        decls: Vec<Decl>,
        body: Vec<Stmt>,
        contains: Vec<ProgramUnit>,
    },

    BlockData {
        name: Option<String>,
        uses: Vec<UseStmt>,
        decls: Vec<Decl>,
    },

    SeparateModuleProcedure {
        name: String,
        // ... same structure as Subroutine/Function
    },
}

enum Prefix {
    Pure,
    Impure,
    Elemental,
    Recursive,
    NonRecursive,
    Module,       // MODULE PROCEDURE prefix
}
```

### 2. Interface Blocks
```fortran
! Explicit interface
interface
    subroutine external_sub(x, n)
        real, intent(in) :: x(:)
        integer, intent(in) :: n
    end subroutine
end interface

! Generic interface
interface operator(+)
    module procedure add_vectors
    module procedure add_scalars
end interface

! Abstract interface
abstract interface
    function integrand(x) result(y)
        real(8), intent(in) :: x
        real(8) :: y
    end function
end interface

! Generic name
interface sort
    module procedure sort_int
    module procedure sort_real
    module procedure sort_char
end interface
```

### 3. Module Structure
```fortran
module my_module
    use other_module, only: some_type
    implicit none
    private                          ! default accessibility

    integer, parameter, public :: MAX_SIZE = 1024

    type, public :: container
        integer :: count
        real, allocatable :: data(:)
    contains
        procedure :: add
        procedure :: get
    end type

    interface
        module subroutine heavy_compute(c)   ! separate module procedure
            type(container), intent(inout) :: c
        end subroutine
    end interface

contains

    subroutine add(self, value)
        class(container), intent(inout) :: self
        real, intent(in) :: value
        ! ...
    end subroutine

    function get(self, index) result(val)
        class(container), intent(in) :: self
        integer, intent(in) :: index
        real :: val
        val = self%data(index)
    end function

end module
```

### 4. USE Statement Variants
```fortran
use my_module                          ! use everything
use my_module, only: foo, bar          ! use only specified
use my_module, only: local => remote   ! rename
use my_module, renamed => original     ! rename without only
use, intrinsic :: iso_c_binding       ! intrinsic module
use, non_intrinsic :: my_module       ! explicit non-intrinsic
```

### 5. IMPORT Statement (F2018)
```fortran
import :: type_name          ! import host entity into interface body
import, all                   ! import all host entities
import, none                  ! import nothing
import, only: name1, name2  ! import specific entities
```

### 6. ENTRY Statement (Legacy)
```fortran
subroutine sub(x)
    real :: x
    ! ...
    return
    entry alt_entry(y)       ! alternative entry point
    real :: y
    ! ...
end subroutine
```

### 7. Statement Functions (Legacy)
```fortran
! Single-line function definition in declaration section
f(x) = x**2 + 2*x + 1
```

Ambiguous with array assignment — resolved by context (appears in declarations, before executable statements).

## Testing Strategy

### Module Parsing
Parse modules with:
- Public/private accessibility
- Derived type definitions with type-bound procedures
- Interface blocks (explicit, generic, abstract)
- Contains section with module subprograms

### Subprogram Parsing
- Subroutines with all argument forms
- Functions with RESULT clause
- PURE, ELEMENTAL, RECURSIVE prefixes
- Internal subprograms (contains within contains)
- BIND(C) subprograms

### Multi-Unit Files
Fortran allows multiple program units in one file:
```fortran
module m1
    ...
end module

module m2
    use m1
    ...
end module

program main
    use m2
    ...
end program
```
Parse and return a list of program units.

### fortsh Module Structure
Parse all 55 fortsh `.f90` files. Verify module dependencies are captured in USE statements.

## Definition of Done
- PROGRAM, SUBROUTINE, FUNCTION, MODULE, SUBMODULE, BLOCK DATA all parse
- Subprogram prefixes (pure, elemental, recursive) parse
- RESULT clause parses
- BIND(C) parses on subprograms
- Interface blocks parse (all forms)
- USE statements parse (all variants)
- IMPORT statements parse
- CONTAINS sections parse with internal subprograms
- Multiple program units per file parse
- ENTRY statements parse
- Statement functions recognized
- All fortsh source files parse completely
- `cargo test` subprogram/module parser tests pass
