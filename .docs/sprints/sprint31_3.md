# Sprint 31.3: Legacy Statement Completeness

## Context
With fixed-form source flowing through the pipeline (sprint 31.2), the next step is implementing the F77 statements that fixed-form programs actually use. These are constructs that were superseded by modern Fortran but still appear in legacy scientific codes, numerical libraries, and F77 test suites.

## Prerequisites
Sprint 31.2 (fixed-form through full pipeline)

## Deliverables

### 1. ENTRY Points
**Problem:** `ENTRY alt_name(args)` inside a SUBROUTINE or FUNCTION creates an alternate entry point with a different argument list. Not parsed or lowered.

**Solution:**
- Parser: recognize ENTRY as a statement inside subroutine/function bodies
- AST: add `Stmt::Entry { name, args }` variant
- Lowering: emit a second function with the ENTRY name that shares the same body from the ENTRY point forward. Local variables before ENTRY are undefined. Variables after ENTRY are shared.
- Codegen: both the primary and ENTRY functions appear as `.globl` symbols

**Complexity:** Medium-high. Shared local state between entry points is the hard part.

**Files:** `src/parser/stmt.rs`, `src/ast/stmt.rs`, `src/ir/lower.rs`

### 2. Arithmetic IF
**Problem:** `IF (expr) label1, label2, label3` — branch based on whether expr is negative, zero, or positive. Parsed but lowering may be incomplete.

**Solution:**
- Verify parser produces `Stmt::ArithmeticIf { expr, neg, zero, pos }`
- Lower to: `CMP expr, 0; B.LT label1; B.EQ label2; B label3`
- Labels must reference valid statement labels in the same scope

**Files:** `src/ir/lower.rs`

### 3. Assigned GOTO
**Problem:** `ASSIGN 10 TO L` stores a label in an integer variable. `GO TO L` branches to the stored label. This is deleted in F95 but still appears in legacy code.

**Solution:**
- Parser: recognize `ASSIGN label TO var` as a statement
- AST: add `Stmt::Assign { label, var }` variant
- Lowering: store the block address corresponding to the label into the variable. `GO TO var` loads and branches.
- This is essentially a computed branch through an integer.

**Complexity:** Medium. The label-to-block-address mapping exists from GOTO support.

**Files:** `src/parser/stmt.rs`, `src/ast/stmt.rs`, `src/ir/lower.rs`

### 4. Labeled DO Loops
**Problem:** `DO 10 I=1,N ... 10 CONTINUE` — the classic F77 loop termination style where the DO loop ends at the labeled statement. The parser may handle `DO label var=start,end` but the label-termination logic needs verification.

**Solution:**
- Verify the parser recognizes `DO label var=start,end[,step]`
- The lowering must connect the labeled CONTINUE (or other statement) as the loop back-edge target
- Shared termination: multiple DO loops can share the same terminating label (deprecated but legal in F77)

**Files:** `src/parser/stmt.rs`, `src/ir/lower.rs`

### 5. Hollerith Constants
**Problem:** `6HFOOBAR` is a Hollerith constant — 6 characters. Used in DATA statements and as arguments. The lexer may recognize them but lowering is untested.

**Solution:**
- Verify the lexer produces a Hollerith token
- Lower as a character string constant
- Handle in DATA statements, FORMAT statements, and as subroutine arguments
- Hollerith in CALL arguments: `CALL SUB(6HFOOBAR)` passes a character argument

**Files:** `src/lexer/fixed.rs`, `src/ir/lower.rs`

### 6. PAUSE Statement
**Problem:** `PAUSE` and `PAUSE 'message'` — halt execution and wait for operator response. Deleted in F95 but present in F77 code.

**Solution:** Lower as a print + read from stdin (or just print and continue, matching gfortran behavior).

**Files:** `src/parser/stmt.rs`, `src/ir/lower.rs`

## Definition of Done
- ENTRY points compile and both entry names are callable
- `IF (X) 10, 20, 30` branches correctly for negative/zero/positive
- `ASSIGN 10 TO L; GO TO L` works
- `DO 10 I=1,10 ... 10 CONTINUE` loops correctly
- Hollerith constants in DATA and CALL compile
- PAUSE prints and continues
- All tests from sprint 31.2 still pass
