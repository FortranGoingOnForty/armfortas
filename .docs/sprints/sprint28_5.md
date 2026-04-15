# Sprint 28.5: Modern Fortran Feature Gaps

## Prerequisites
Sprint 28 (Derived Types & OOP)

## Goals
Address remaining modern Fortran features that were deferred as exotic but become relevant once OOP and advanced types land.

## Deliverables

### 1. SELECT TYPE (F2003)
Parser, AST, sema validation, and IR lowering for `SELECT TYPE` with type guards (`TYPE IS`, `CLASS IS`, `CLASS DEFAULT`). Depends on Sprint 28's polymorphism infrastructure.

### 2. AssumedRank `(..)` (F2018)
Fix lexer to handle bare `..` token. Parser produces `ArraySpec::AssumedRank`. Sema validates it only appears in dummy arguments.

### 3. DO CONCURRENT LocalitySpec (F2018)
Parse `LOCAL`, `LOCAL_INIT`, `SHARED`, `DEFAULT(NONE)`, `REDUCE` clauses. Lower as sequential loop (optimization to parallel deferred).

### 4. BOZ Literal Type Context
BOZ literals (`B'1010'`, `Z'FF'`) get their type from context (e.g., `real :: x = Z'3F800000'`). Implement context-dependent typing in sema and IR lowering.

### 5. STOP QUIET= Specifier (F2018)
Parse the `QUIET=` specifier on STOP/ERROR STOP. Pass the flag to the runtime.

### 6. Submodule Resolution
Implement submodule parent/ancestor lookup in symbol table. Wire into `resolve_unit` for `ProgramUnit::Submodule`.

## Definition of Done
- SELECT TYPE compiles and runs with at least one polymorphic test
- All features parse without error on valid input
- Existing tests unaffected
