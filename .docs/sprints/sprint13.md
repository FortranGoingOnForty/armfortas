# Sprint 13: Semantic Analysis — Type System

## Prerequisites
Sprint 12 (symbol tables)

## Goals
Implement Fortran's type system: type representation, type checking for expressions, implicit conversions, array shape analysis, and argument matching for subprogram calls. This is where `A(I)` finally gets resolved as an array access vs. a function call.

## Deliverables

### 1. Type Representation
```rust
enum Type {
    // Intrinsic types
    Integer { kind: u8 },        // kind=1,2,4,8 (bytes)
    Real { kind: u8 },           // kind=4 (single), 8 (double), 16 (quad)
    Complex { kind: u8 },        // kind=4, 8, 16
    Logical { kind: u8 },        // kind=1,2,4,8
    Character { kind: u8, len: CharLen },

    // Derived types
    Derived { name: String, def: DerivedTypeDef },

    // Special
    Procedure { interface: ProcInterface },
    ClassOf { base_type: Box<Type> },   // CLASS(t) — polymorphic
    UnlimitedPoly,                       // CLASS(*)
    AssumedType,                         // TYPE(*)
    Void,                                // for subroutines (no return)
}

enum CharLen {
    Known(i64),       // character(len=10)
    Assumed,          // character(len=*)
    Deferred,         // character(len=:)
    Expr(ExprId),     // character(len=n) — runtime expression
}

struct ArrayType {
    element_type: Type,
    rank: u8,                    // 1-15 (F2018 allows up to 15)
    shape: ArrayShape,
}

enum ArrayShape {
    Explicit(Vec<Dimension>),     // known bounds
    AssumedShape(u8),             // rank known, bounds from actual argument
    AssumedSize,                  // last dimension is *
    Deferred(u8),                 // allocatable — rank known, bounds at runtime
    AssumedRank,                  // dimension(..) — rank and shape unknown
}

struct Dimension {
    lower: Bound,
    upper: Bound,
}

enum Bound {
    Constant(i64),
    Runtime(ExprId),    // determined at runtime
}
```

### 2. Expression Type Checking
Walk expression ASTs and compute result types:

**Arithmetic promotion rules:**
| Left | Op | Right | Result |
|------|----|-------|--------|
| integer(k1) | + | integer(k2) | integer(max(k1,k2)) |
| integer | + | real | real |
| real | + | complex | complex |
| integer(4) | + | real(8) | real(8) |

The rules: when mixing types, promote to the "wider" type. When mixing kinds, promote to the larger kind.

**Power operator** special case: `integer ** integer → integer`, but `real ** integer → real` (integer exponent is special).

**Comparison operators**: Always produce `logical(4)` regardless of operand types.

**Concatenation**: `character // character → character` (lengths add).

**Logical operators**: `.and.`, `.or.`, `.not.` — operands must be logical, result is logical.

### 3. Implicit Conversion Insertion
When types don't match, insert conversion nodes:
```fortran
real :: x
integer :: i
x = i              ! implicit INT→REAL conversion
```

The semantic analyzer inserts a `Convert` node in the typed AST:
```rust
// Before: Assignment { target: x, value: Name("i") }
// After:  Assignment { target: x, value: Convert { from: Integer(4), to: Real(4), expr: Name("i") } }
```

### 4. Array/Function/Substring Disambiguation
This is the big moment. Given `A(I)`:
1. Look up `A` in symbol table
2. If `A` is an array → `ArrayElement` node
3. If `A` is a function → `FunctionCall` node
4. If `A` is a character variable and subscript is `start:end` → `Substring` node
5. If `A` is a generic interface → resolve to specific function based on argument types
6. If `A` is unknown and implicit typing applies → depends on context (likely variable)

### 5. Subprogram Call Checking
When a subroutine or function is called:
1. Resolve the callee (by name, or through generic interface dispatch)
2. Match actual arguments to dummy arguments (positional and keyword)
3. Check types compatibility (actual must match dummy's type/kind/rank)
4. Check intent compatibility (can't pass a constant to `intent(out)`)
5. Check array shape compatibility
6. Check optional argument handling

```fortran
interface
    subroutine process(data, n, verbose)
        real, intent(inout) :: data(:)
        integer, intent(in) :: n
        logical, intent(in), optional :: verbose
    end subroutine
end interface

call process(my_array, 100)                    ! ok: verbose is optional
call process(my_array, 100, verbose=.true.)    ! ok: keyword arg
call process(100, my_array)                    ! error: type mismatch
```

### 6. Generic Resolution
```fortran
interface swap
    subroutine swap_int(a, b)
        integer, intent(inout) :: a, b
    end subroutine
    subroutine swap_real(a, b)
        real, intent(inout) :: a, b
    end subroutine
end interface

call swap(i, j)     ! resolves to swap_int (both integer)
call swap(x, y)     ! resolves to swap_real (both real)
call swap(i, x)     ! error: no matching specific
```

Resolution: find the specific procedure whose dummy argument types match the actual argument types. Exactly one must match.

### 7. Intrinsic Function Resolution
Intrinsic functions are generic with special rules:
- `abs(integer) → integer`, `abs(real) → real`, `abs(complex) → real`
- `max(a, b, c, ...)` — any number of arguments, all same type
- `reshape(source, shape)` — result rank determined by `shape` argument

Build an intrinsic function table with type signatures for all ~400 intrinsics.

## Testing Strategy

### Type Arithmetic Tests
Verify correct result types for all combinations of operand types and operators.

### Conversion Tests
Verify implicit conversions are inserted where needed and not where not needed.

### Disambiguation Tests
```fortran
real :: a(10)
a(3) = 5.0          ! array access
x = sin(3.14)       ! function call
s(1:5) = 'hello'    ! substring
```

### Generic Resolution Tests
Multiple specific procedures, verify correct dispatch based on argument types.

### Intrinsic Tests
Type-check calls to common intrinsics: `abs`, `max`, `sin`, `trim`, `len`, `size`, `reshape`, `matmul`.

### Error Detection Tests
- Type mismatch in assignment
- Wrong argument types in call
- Ambiguous generic resolution
- Rank mismatch in array operations
- Non-optional argument missing

### fortsh Type Checking
Run type checking on fortsh source. All valid code must pass. Flag any issues for investigation (likely parser or symbol table bugs).

## Definition of Done
- All intrinsic types represented with kind
- Arithmetic promotion rules correct
- Implicit conversions inserted
- Array/function/substring disambiguation works
- Subprogram call argument matching works
- Generic interface resolution works
- Intrinsic function table covers all F2018 intrinsics
- Type errors produce clear diagnostic messages
- fortsh source passes type checking
- `cargo test` type system tests pass
