# Sprint 31.2: Fixed-Form Through Full Pipeline

## Context
The fixed-form lexer exists (`src/lexer/fixed.rs`) and can tokenize column-based Fortran. But fixed-form source has never been tested through the full pipeline: parse → sema → IR → codegen → binary. Real F77 programs (and many legacy scientific codes) are fixed-form. This sprint gets them compiling and running.

## Prerequisites
Sprint 31.1 (--std= gating, F77 mode basics)

## Deliverables

### 1. Fixed-Form Detection and Dispatch
**Problem:** The driver doesn't auto-detect fixed-form from file extension or switch the lexer mode.

**Solution:**
- `.f`, `.for`, `.ftn` extensions → fixed-form
- `.f90`, `.f95`, `.f03`, `.f08`, `.f18` → free-form
- `--fixed-form` / `--free-form` flags override extension detection
- Thread source form through to the lexer call

**Files:** `src/driver/mod.rs`, `src/lexer/mod.rs`

### 2. Fixed-Form Lexer Hardening
**Problem:** The fixed-form lexer handles basics but hasn't been stress-tested with real code.

**Verify and fix:**
- Column 1-5: label field (numeric labels)
- Column 6: continuation character (any non-blank, non-zero)
- Column 7-72: statement field
- Column 73+: ignored (comment/sequence number)
- `C`, `c`, `*` in column 1: full-line comment
- Blank lines
- Mixed tabs and spaces in label field
- Hollerith constants (`6Hfoobar`)

**Files:** `src/lexer/fixed.rs`

### 3. End-to-End Fixed-Form Tests
**Solution:** Create a set of fixed-form test programs that exercise the full pipeline:

```fortran
      PROGRAM HELLO
      PRINT *, 'HELLO WORLD'
      END
```

Programs to test:
- Basic hello world (columns 7-72)
- Continuation lines (column 6)
- Statement labels and GOTO
- DO loops with labeled termination (`DO 10 I=1,10 ... 10 CONTINUE`)
- COMMON blocks
- Subroutine calls
- Simple arithmetic
- Character strings spanning continuation lines
- Mixed-case (Fortran is case-insensitive, but F77 code is often ALL CAPS)

**Files:** `test_programs/fixed_*.f` (note: `.f` extension)

### 4. Parser Tolerance for Fixed-Form Idioms
**Problem:** Fixed-form allows constructs that free-form doesn't:
- No `::` in declarations (`INTEGER X` instead of `INTEGER :: X`)
- Spaces within keywords (`END DO` vs `ENDDO`, `GO TO` vs `GOTO`)
- Labels on any statement

**Verify** the parser handles these when the lexer produces the right tokens.

## Definition of Done
- `afs --fixed-form hello.f -o hello && ./hello` prints "HELLO WORLD"
- `.f` extension auto-selects fixed-form
- ≥5 fixed-form test programs compile and run correctly
- Continuation lines work across at least 3 continuation levels
- COMMON blocks, labeled DO, GOTO all work in fixed-form
