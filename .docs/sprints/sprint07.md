# Sprint 7: Parser — Expressions

## Prerequisites
Sprint 5 (free-form lexer)

## Goals
Build the foundation of the recursive descent parser: expression parsing with correct precedence, associativity, and support for all Fortran expression forms. Expressions are the atoms of every statement — getting this right first makes everything else easier.

## Deliverables

### 1. AST Expression Nodes
```rust
enum Expr {
    // Literals
    IntegerLiteral { value: i64, kind: Option<Kind> },
    RealLiteral { value: f64, kind: Option<Kind> },
    ComplexLiteral { real: Box<Expr>, imag: Box<Expr> },
    StringLiteral { value: String, kind: Option<Kind> },
    LogicalLiteral { value: bool, kind: Option<Kind> },
    BozLiteral { value: u64, base: BozBase },

    // Names and access
    Name { name: String },
    ArrayElement { base: Box<Expr>, subscripts: Vec<Expr> },
    ArraySection { base: Box<Expr>, subscripts: Vec<SectionSubscript> },
    ComponentAccess { base: Box<Expr>, component: String },  // x%field
    Substring { base: Box<Expr>, start: Option<Box<Expr>>, end: Option<Box<Expr>> },

    // Operations
    UnaryOp { op: UnaryOp, operand: Box<Expr> },
    BinaryOp { op: BinaryOp, left: Box<Expr>, right: Box<Expr> },

    // Calls (ambiguous with array access until semantic analysis)
    FunctionCall { name: Box<Expr>, args: Vec<Argument> },

    // Special forms
    ArrayConstructor { values: Vec<AcValue> },  // [1, 2, 3] or (/ 1, 2, 3 /)
    ImpliedDo { values: Vec<AcValue>, var: String, start: Box<Expr>, end: Box<Expr>, step: Option<Box<Expr>> },
    ParenExpr { inner: Box<Expr> },
}

enum SectionSubscript {
    Element(Expr),
    Range { start: Option<Expr>, end: Option<Expr>, stride: Option<Expr> },
}

struct Argument {
    keyword: Option<String>,  // for keyword arguments: name=value
    value: Expr,
}
```

### 2. Operator Precedence
Fortran operator precedence (highest to lowest):

| Precedence | Operator | Associativity |
|------------|----------|---------------|
| 1 | defined unary operator (`.myop.`) | right |
| 2 | `**` (power) | **right** |
| 3 | `*`, `/` (multiply, divide) | left |
| 4 | unary `+`, `-` | right |
| 5 | binary `+`, `-` | left |
| 6 | `//` (concatenation) | left |
| 7 | `.eq.`/`==`, `.ne.`/`/=`, `.lt.`/`<`, `.le.`/`<=`, `.gt.`/`>`, `.ge.`/`>=` | non-associative |
| 8 | `.not.` | right |
| 9 | `.and.` | left |
| 10 | `.or.` | left |
| 11 | `.eqv.`, `.neqv.` | left |
| 12 | defined binary operator (`.myop.`) | left |

Implementation: Pratt parser (operator precedence parsing) — cleaner than the classic recursive descent chain for expressions and easy to extend with defined operators.

### 3. The Array/Function Ambiguity
In Fortran, `A(I)` could be:
- Array element access
- Function/subroutine call
- Substring reference

The parser **cannot distinguish these syntactically**. All are parsed as `FunctionCall` or `ArrayElement` and disambiguated during semantic analysis based on how `A` was declared.

```fortran
real :: a(10)
a(3) = 5.0          ! array access

character(10) :: s
s(1:5) = 'hello'    ! substring

x = sin(3.14)       ! function call
```

The parser produces a unified `FunctionCall` node; sema resolves it.

### 4. Array Constructors
Two syntax forms:
```fortran
x = [1, 2, 3, 4, 5]           ! F2003 bracket form
x = (/ 1, 2, 3, 4, 5 /)       ! F90 form
x = [(i, i=1,10)]              ! implied do
x = [integer :: 1.0, 2.0]     ! typed constructor (F2003)
```

### 5. Component Access Chains
```fortran
x%component
x%inner%deep%field
array(i)%component
func()%component
```
Left-to-right associative, mixed with array access and function calls.

## Testing Strategy

### Precedence Tests
For every precedence level, verify that `a OP1 b OP2 c` parses with correct tree structure:
```rust
// a + b * c → Add(a, Mul(b, c))
// a ** b ** c → Pow(a, Pow(b, c))  // right-associative!
// a .and. b .or. c → Or(And(a, b), c)
```

### Parenthesization Tests
Parse expressions, pretty-print with full parentheses, verify:
```
a + b * c → (a + (b * c))
a ** b ** c → (a ** (b ** c))
-a ** b → (-(a ** b))  // unary minus below power
```

### Literal Tests
Every literal form parses correctly:
- `42`, `42_8`, `42_int64`
- `3.14`, `3.14d0`, `1.0e5_8`, `.5`, `5.`
- `'hello'`, `"hello"`, `'it''s'`
- `.true.`, `.false.`, `.true._4`
- `B'1010'`, `O'777'`, `Z'FF'`

### Complex Expression Tests
Parse expressions from fortsh source — extract expressions from assignments, if conditions, function arguments, and verify they parse without error.

## Definition of Done
- All Fortran expression forms parse correctly
- Operator precedence matches the Fortran standard exactly
- Right-associativity of `**` handled
- Array constructors (both forms) parse
- Component access chains parse
- Implied-do in array constructors parses
- Keyword arguments in function calls parse
- Expressions from fortsh source files parse without error
- `cargo test` expression parser tests pass
