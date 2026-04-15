# Sprint 8: Parser — Declarations

## Prerequisites
Sprint 7 (expression parser)

## Goals
Parse all Fortran declaration statements. Declarations define the types, shapes, and attributes of variables, and they're the most attribute-heavy part of Fortran's syntax. This sprint handles every way a Fortran programmer can declare data.

## Deliverables

### 1. AST Declaration Nodes
```rust
enum Decl {
    // Type declarations
    TypeDecl {
        type_spec: TypeSpec,
        attrs: Vec<Attribute>,
        entities: Vec<EntityDecl>,
    },

    // Implicit
    ImplicitNone { external: bool, type_: bool },
    ImplicitStmt { specs: Vec<ImplicitSpec> },

    // Derived type definition
    DerivedTypeDef {
        name: String,
        extends: Option<String>,
        attrs: Vec<TypeAttr>,
        components: Vec<ComponentDecl>,
        type_bound_procs: Vec<TypeBoundProc>,
        final_procs: Vec<String>,
    },

    // Old-style
    ParameterStmt { pairs: Vec<(String, Expr)> },
    CommonBlock { name: Option<String>, vars: Vec<String> },
    EquivalenceStmt { groups: Vec<Vec<Expr>> },
    DataStmt { sets: Vec<DataSet> },

    // Enum
    EnumDef { enumerators: Vec<(String, Option<Expr>)> },

    // Use
    UseStmt { module: String, nature: UseNature, renames: Vec<Rename>, only: Option<Vec<OnlyItem>> },
}
```

### 2. Type Specifiers
```rust
enum TypeSpec {
    Integer(Option<KindSelector>),
    Real(Option<KindSelector>),
    DoublePrecision,
    Complex(Option<KindSelector>),
    DoubleComplex,
    Logical(Option<KindSelector>),
    Character(Option<CharSelector>),
    Type(String),          // TYPE(my_type)
    Class(String),         // CLASS(my_type) — polymorphic
    ClassStar,             // CLASS(*) — unlimited polymorphic
    TypeStar,              // TYPE(*) — assumed type
}

enum KindSelector {
    // integer(4), integer(kind=4), integer*4
    Expr(Expr),
}

enum CharSelector {
    // character(10), character(len=10, kind=1), character*10
    LenAndKind { len: Option<LenSpec>, kind: Option<Expr> },
}

enum LenSpec {
    Expr(Expr),
    Star,          // character(len=*) — assumed length
    Colon,         // character(len=:) — deferred length
}
```

### 3. Attributes
All declaration attributes:
```rust
enum Attribute {
    Dimension(Vec<ArraySpec>),    // dimension(10), dimension(:,:)
    Allocatable,
    Pointer,
    Target,
    Intent(Intent),               // intent(in), intent(out), intent(inout)
    Optional,
    Save,
    Parameter,
    Value,                        // pass by value (C interop)
    Volatile,
    Asynchronous,
    Protected,
    Contiguous,                   // F2008
    Codimension(Vec<CoarraySpec>),
    External,
    Intrinsic,
    Bind(Option<String>),         // bind(c, name="cfunc")
    AccessSpec(Access),           // public, private
}

enum ArraySpec {
    Explicit { lower: Option<Expr>, upper: Expr },
    AssumedShape { lower: Option<Expr> },          // dimension(:)
    AssumedSize { lower: Option<Expr> },            // dimension(*)
    Deferred,                                        // dimension(:) + allocatable
    AssumedRank,                                     // dimension(..)  F2018
}
```

### 4. Entity Declarations
```fortran
! All of these must parse:
integer :: x, y, z
integer :: x = 0, y = 1
integer, dimension(10) :: a
integer :: a(10), b(20,30)
real(8), allocatable, intent(in) :: matrix(:,:)
character(len=:), allocatable :: name
character(len=*), intent(in) :: input
type(my_type), pointer :: ptr => null()
```

### 5. Old-Style Declarations
```fortran
! F77 style — must still parse
integer x, y
real*8 value
character*20 name
dimension a(10,10)
common /block1/ x, y, z
equivalence (a(1), b(1))
data x /1.0/, y /2.0/
parameter (pi = 3.14159265)
implicit double precision (a-h, o-z)
```

### 6. Derived Type Definitions
```fortran
type :: particle
    real(8) :: x, y, z
    real(8) :: vx, vy, vz
    real(8) :: mass
    character(len=:), allocatable :: name
contains
    procedure :: kinetic_energy
    procedure :: distance_to => calc_distance
    generic :: operator(+) => add_particles
    final :: cleanup
end type
```

## Testing Strategy

### Declaration Parsing Tests
Parse every declaration form listed above, verify AST structure.

### Attribute Combination Tests
Verify that multiple attributes combine correctly:
```fortran
real(8), dimension(:,:), allocatable, intent(inout) :: matrix
```

### Old-Style vs New-Style
Verify both styles produce equivalent AST nodes.

### Derived Type Tests
- Simple types (only components)
- Types with type-bound procedures
- Types with extends
- Types with private/public components
- Types with final subroutines

### fortsh Declaration Survey
Extract and parse all declarations from fortsh source. Verify no parse errors. This exercises real-world declaration patterns.

## Key Technical Notes

### The Attribute Parsing Challenge
Fortran allows attributes in two positions:
```fortran
integer, allocatable :: x(:)    ! attribute on the type declaration line
allocatable :: x                 ! standalone attribute statement (F77 style)
```

Both must be handled and produce the same semantic result.

### Character Length Pitfalls
Character declarations have multiple syntaxes:
```fortran
character(10) :: s          ! len=10
character(len=10) :: s      ! same
character*10 s              ! F77 style, same
character(len=:) :: s       ! deferred length (must be allocatable or pointer)
character(len=*) :: s       ! assumed length (dummy argument)
```

### Double Colon
The `::` is optional in some contexts:
```fortran
integer :: x     ! with ::
integer x        ! without :: (F77 style)
```
But required when attributes or initialization are present:
```fortran
integer, allocatable :: x(:)    ! :: required
integer :: x = 5                ! :: required
```

## Definition of Done
- All type specifiers parse (intrinsic + derived types)
- All attributes parse individually and in combination
- Old-style and new-style declarations both parse
- Derived type definitions parse with all features
- IMPLICIT statements parse (including IMPLICIT NONE variants)
- USE statements parse (with ONLY, renames)
- COMMON, EQUIVALENCE, DATA, PARAMETER parse
- Declarations from fortsh source parse without error
- `cargo test` declaration parser tests pass
