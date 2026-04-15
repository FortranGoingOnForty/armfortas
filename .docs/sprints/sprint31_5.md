# Sprint 31.5: Legacy Edge Cases & Robustness

## Context
After 31.0–31.3 deliver the major features (implicit none, generic interfaces, --std= gating, fixed-form, legacy statements), this sprint sweeps up edge cases, hardens the implementation, and stress-tests against external legacy code. This is the stabilization pass before moving to CLI polish (sprint 32) and fortsh (sprints 33–34).

## Prerequisites
Sprint 31.3 (legacy statement completeness)

## Deliverables

### 1. EQUIVALENCE with Mixed Types
**Problem:** `EQUIVALENCE (A, B)` where A is REAL and B is INTEGER overlays the same memory. The lowering needs to handle type punning correctly (use the same alloca, cast at access).

**Files:** `src/ir/lower.rs`

### 2. Hollerith in DATA Statements
**Problem:** `DATA X /4HABCD/` stores a Hollerith constant into a variable via DATA. The DATA lowering needs to handle Hollerith as a character initializer that may be stored into non-character variables (type punning).

**Files:** `src/ir/lower.rs`

### 3. Column Edge Cases in Fixed-Form
- Tabs in column 1-6 (common vendor extension)
- Lines shorter than 6 characters
- Lines exactly 72 characters (no truncation bug)
- Lines longer than 72 characters (columns 73+ must be ignored)
- Empty continuation lines
- Labels with leading zeros (`007` = label 7)

**Files:** `src/lexer/fixed.rs`

### 4. Coarray Syntax Stubs
**Problem:** Coarray syntax (`x[i]`, `sync all`, `co_sum`) appears in F2008+ code. We don't implement coarrays, but we should parse the syntax without crashing so that the compiler can give a clear "not implemented" diagnostic.

**Solution:** Recognize `[...]` after variable names as coarray subscripts. Parse SYNC ALL, SYNC IMAGES, etc. as statements. Emit "coarray features not yet supported" diagnostic.

**Files:** `src/parser/expr.rs`, `src/parser/stmt.rs`

### 5. External Legacy Code Compilation
**Smoke test** the compiler against:
- BLAS Level 1 (daxpy, ddot, dnrm2) — ~200 lines of F77
- LINPACK dgefa/dgesl — ~400 lines of F77
- A small Numerical Recipes routine

These don't need to produce correct results — just compile without crashes. Failures become bug reports for future sprints.

### 6. Implied DO in I/O Lowering Verification
**Problem:** The parser handles `(a(i), i=1,n)` in PRINT/WRITE, producing an ArrayConstructor with ImpliedDo. Verify the lowering emits correct loop code and the output matches gfortran.

**Files:** `src/ir/lower.rs`

## Definition of Done
- EQUIVALENCE with mixed REAL/INTEGER compiles
- Fixed-form edge cases (tabs, short lines, long lines) don't crash
- Coarray syntax produces a diagnostic instead of a crash
- At least 1 BLAS routine compiles from fixed-form source
- `print *, (a(i), i=1,5)` produces correct output
- All 279+ test programs still pass
