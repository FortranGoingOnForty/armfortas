# Sprint 5: Lexer — Free-Form

## Prerequisites
Sprint 4 (preprocessor — lexer consumes preprocessed output)

## Goals
Tokenize free-form Fortran source into a token stream. This is the compiler's first real Fortran-specific component. Fortran lexing has several unique challenges that make it harder than lexing C-family languages.

## Deliverables

### 1. Token Types
```rust
enum TokenKind {
    // Literals
    IntegerLiteral,       // 42, 0, 1_8 (with kind)
    RealLiteral,          // 3.14, 1.0d0, 6.022e23, 1.0_8
    ComplexLiteral,       // (1.0, 2.0) — handled at parser level
    StringLiteral,        // 'hello' or "hello"
    BozLiteral,           // B'1010', O'777', Z'FF'
    LogicalLiteral,       // .true., .false., .true._4

    // Identifiers and keywords
    Identifier,           // variable names, type names
    Keyword(Keyword),     // program, subroutine, integer, etc.

    // Operators
    Plus, Minus, Star, Slash, Power,     // + - * / **
    Concat,                               // //
    Eq, Ne, Lt, Gt, Le, Ge,             // == /= < > <= >=
    DotEq, DotNe, DotLt, DotGt, DotLe, DotGe,  // .eq. .ne. .lt. .gt. .le. .ge.
    DotAnd, DotOr, DotNot, DotEqv, DotNeqv,     // .and. .or. .not. .eqv. .neqv.
    DotTrue, DotFalse,                            // .true. .false.
    DefinedOp(String),                             // .myop.

    // Punctuation
    LParen, RParen,       // ( )
    LBracket, RBracket,   // [ ]  (array constructor, F2003+)
    Comma, Colon, Semicolon,
    ColonColon,           // ::
    Percent,              // % (component access)
    Arrow,                // => (pointer assignment, rename)
    Ampersand,            // & (continuation)
    Assign,               // =

    // Special
    Newline,              // significant in Fortran (statement terminator)
    Comment,              // ! to end of line
    Eof,
}
```

### 2. Keyword Recognition
Fortran keywords are **not reserved** — a variable can be named `integer` or `do`. This means keyword recognition is context-dependent:

```fortran
integer integer          ! valid: declares a variable named "integer"
do do = 1, 10           ! valid: do loop with variable named "do"
if (if) then            ! valid: if statement testing variable named "if"
```

Strategy: Lex everything as `Identifier`. The parser determines from context whether an identifier is a keyword. The lexer provides a `is_keyword(name) -> Option<Keyword>` helper but doesn't commit to the classification.

### 3. String Literals
Two styles, both must handle:
- `'single quoted'` and `"double quoted"`
- Escaped quotes by doubling: `'it''s'` → `it's`
- Continuation across lines with `&`:
  ```fortran
  character(*), parameter :: msg = 'hello &
       &world'
  ```

### 4. Continuation Lines
Free-form continuation: `&` at end of line, optionally `&` at start of continuation:
```fortran
x = a + b + &
    c + d
! or
x = a + b + &
    &c + d
```

The lexer must join continued lines transparently, producing tokens as if the statement were on one line, while maintaining source location tracking.

### 5. Numeric Literals
Fortran numeric literals are more complex than most languages:
- Integer: `42`, `42_8` (with kind suffix), `42_int64`
- Real: `3.14`, `3.14d0` (double), `1.0e5`, `1.0d5`, `3.14_8`, `.5`, `5.`
- BOZ: `B'1010'`, `O'777'`, `Z'FF'`, `B"1010"` (binary, octal, hex)

Kind suffixes can be integer literals or named constants — resolution happens at semantic analysis, but the lexer must capture the suffix text.

### 6. Source Location Tracking
Every token carries its source location:
```rust
struct Token {
    kind: TokenKind,
    text: String,          // original text
    span: Span,
}

struct Span {
    file_id: FileId,       // which source file
    start: Position,       // line, column
    end: Position,
}
```

These map back through the preprocessor's source map to original file locations.

## Testing Strategy

### Unit Tests
- Lex individual token types: each literal form, each operator, each punctuation
- Lex continuation lines and verify joined token stream
- Lex strings with embedded quotes and continuations
- Lex numeric literals with kind suffixes
- Lex BOZ constants

### Fortsh Tokenization
The ultimate test: tokenize every `.f90` file in the fortsh codebase.
- No lexer errors
- Token count and types are reasonable
- Round-trip: tokens → text → tokens produces identical token stream

### Ambiguity Tests
```fortran
! These must all lex correctly:
real :: x = 1.0           ! 'real' as keyword, '1.0' as real literal
x = real(i)               ! 'real' as identifier (function call)
if (x > 1.0) y = 2       ! single-line if (no 'then')
do i = 1, 10              ! 'do' as keyword
do = 3.14                 ! 'do' as identifier (variable name)
```

### Performance Test
Lex all ~57K lines of fortsh in under 1 second.

## Key Technical Notes

### The Whitespace Problem
In Fortran, whitespace within tokens is historically ignored in fixed-form:
```fortran
      DO 10 I = 1, 10      ! "DO 10 I" or "DO10I"?
      GO TO 100             ! "GOTO" or "GO TO"?
```
This is a **fixed-form** problem (Sprint 6). In free-form, whitespace is significant as a token separator, making lexing much more conventional. Sprint 5 only handles free-form.

### The .operator. Problem
Tokens like `.and.` and `.true.` look like dotted operators, but so do user-defined operators (`.myop.`). The lexer recognizes known dotted tokens and produces `DefinedOp` for unknown ones. The parser/sema validates whether a defined operator actually exists.

### Lookahead
Some tokens require lookahead:
- `**` vs `*` `*` — is it power or two multiply operators? (Always power in Fortran)
- `//` vs `/` `/` — concatenation vs two divides? (Always concatenation)
- `::` vs `:` `:` — always double colon when two colons adjacent
- `=>` vs `=` `>` — always arrow

These are unambiguous in Fortran (unlike some languages) but require 1-character lookahead.

## Definition of Done
- All token types correctly recognized
- Continuation lines handled transparently
- All numeric literal forms (integer, real, BOZ with kind suffixes) lex correctly
- String literals with embedded quotes and continuations lex correctly
- Source locations accurate through continuations
- All fortsh `.f90` files tokenize without errors
- Performance: fortsh tokenized in < 1 second
- `cargo test` lexer tests pass
