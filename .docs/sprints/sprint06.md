# Sprint 6: Lexer — Fixed-Form & Multi-Mode

## Prerequisites
Sprint 5 (free-form lexer works)

## Goals
Add fixed-form (F77-style) tokenization and automatic mode detection. This completes the lexer — after this sprint, we can tokenize any Fortran source file regardless of era.

## Deliverables

### 1. Fixed-Form Column Rules
Fixed-form source has strict column semantics:
- **Column 1**: `C`, `c`, `*`, or `!` → entire line is a comment
- **Columns 1-5**: Statement label (integer, right-justified)
- **Column 6**: Any non-space, non-zero character → continuation of previous line
- **Columns 7-72**: Statement body
- **Columns 73+**: Ignored (historically used for card sequence numbers)

```fortran
C     This is a comment
      PROGRAM HELLO
      INTEGER I
      DO 10 I = 1, 10
         WRITE(*,*) I
   10 CONTINUE
      STOP
      END
```

### 2. Whitespace Insensitivity
The hardest part of fixed-form lexing. In fixed-form Fortran, **whitespace within the statement body is not significant**:

```fortran
      DO10I=1,10        ! same as: DO 10 I = 1, 10
      GOTO100            ! same as: GO TO 100
      REAL*8X            ! same as: REAL*8 X
      DOUBLEPRECISION Y  ! same as: DOUBLE PRECISION Y
```

This means the lexer cannot use whitespace as a token separator. Instead, it must use knowledge of Fortran's syntax to determine token boundaries. This is the single hardest lexing problem in any mainstream language.

**Strategy**: When in fixed-form mode, strip all whitespace from the statement body (after joining continuations), then apply a context-sensitive tokenizer that tries to match the longest known token at each position:
1. Try to match a keyword at the current position
2. Try to match a numeric literal
3. Try to match an operator (`.EQ.`, `.AND.`, etc.)
4. Fall back to identifier characters

The infamous `DO`/assignment ambiguity:
```fortran
      DO 10 I = 1, 10    ! DO loop: DO label var = start, end
      DO 10 I = 1.10     ! Assignment: DO10I = 1.10
```
These are identical until the scanner reaches the comma vs period. This requires lookahead to the end of the statement.

### 3. Hollerith Constants
Fixed-form era feature:
```fortran
      X = 6HFOOBAR       ! 6-character Hollerith constant "FOOBAR"
      CALL SUB(4HABCD)
```
Format: `nH` followed by exactly `n` characters (including spaces). The lexer must count characters exactly.

### 4. Tab-Form Extension
Many compilers accept tab-format as a de facto extension:
- Tab in columns 1-6 → jump to column 7
- Tab followed by digit 1-9 → continuation line
- Tab followed by other → start of statement at column 7

We support this as an extension (warn with `-pedantic`).

### 5. Mode Detection
Automatic detection of source form:
- `.f90`, `.f95`, `.f03`, `.f08`, `.f18` → free-form
- `.f`, `.for`, `.ftn`, `.fpp` → fixed-form
- `-ffixed-form` / `-ffree-form` CLI flags override extension-based detection
- `!$` in column 1-2 triggers conditional compilation (OpenMP sentinel) — recognize but skip for now

### 6. Unified Token Stream
Both modes produce the same `Token` type. The parser doesn't need to know which mode was used — it sees the same token stream either way.

## Testing Strategy

### Classic Fortran Programs
Lex well-known F77 programs:
- BLAS/LAPACK source files (the canonical fixed-form Fortran)
- Netlib programs
- Any F77 code in `.refs/`

### The DO Ambiguity
Dedicated test suite for the `DO`/assignment ambiguity:
```fortran
      DO 10 I = 1, 10    ! must lex as DO loop
      DO 10 I = 1.10     ! must lex as assignment to DO10I
      DO 10 I = 1        ! assignment to DO10I (no comma → not a loop)
```

### Hollerith Tests
```fortran
      X = 3HABC
      CALL F(0HX, 5HHELLO)
```

### Round-Trip with Free-Form
Take a free-form program, convert to fixed-form manually, lex both, verify equivalent token streams (minus source locations).

### Whitespace Insensitivity Stress Test
```fortran
      DOUBLEPRECISIONFUNCTION REALFUN(INTEGERI)
      DOUBLEPRECISION REALFUN
      INTEGERI
      REALFUN=REAL(INTEGERI)*2.0D0
      RETURN
      END
```
Must produce correct tokens despite zero whitespace.

## Key Technical Notes

### Why This Is Hard
The combination of whitespace insensitivity and the DO ambiguity means that fixed-form Fortran is arguably the hardest language to lex correctly. Even GCC's gfortran lexer gets edge cases wrong occasionally.

Our approach: **two-pass lexing** for fixed-form.
1. First pass: join continuation lines, strip columns 73+, identify comment lines
2. Second pass: tokenize the joined, stripped statement body with lookahead

### Performance Consideration
Fixed-form lexing is inherently slower than free-form due to the lookahead requirements. This is acceptable — fixed-form code is legacy, and the performance difference is small on modern hardware.

### Interaction with Preprocessor
Preprocessor runs first and produces free-form or fixed-form output depending on the input. The lexer receives the preprocessed text with source map annotations. Column counting must account for any transformations the preprocessor made.

## Definition of Done
- Fixed-form Fortran lexes correctly with proper column handling
- Whitespace insensitivity handled for all known patterns
- DO/assignment ambiguity resolved correctly
- Hollerith constants recognized
- Tab-form extension supported
- Mode auto-detection by file extension
- CLI flags override auto-detection
- Classic F77 programs (BLAS/LAPACK samples) lex without errors
- `cargo test` all lexer tests pass
