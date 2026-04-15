# Sprint 31.0: Audit Residuals & Modern Fortran Gaps

## Context
Two brutal audits across sprints 30–30.7 surfaced pre-existing gaps that block any real Fortran code: undeclared variables compile silently under `implicit none`, generic interfaces don't resolve, and compile-time kind intrinsics are missing. These aren't module-system issues — they're foundational sema/lowering gaps that existed before sprint 30 but became visible once real programs hit the compiler. Fixing them before moving to `--std=` gating or fixed-form is the right order because every future sprint benefits.

## Prerequisites
Sprint 30.7 (module system, intrinsic modules, audit fixes)

## Deliverables

### 1. IMPLICIT NONE Enforcement
**Problem:** `implicit none` is parsed and tracked in the symbol table, but undeclared variables compile silently to zero-initialized locals. This masks typos and defeats the purpose of the most common Fortran safety feature.

**Solution:** Add a validation pass in `validate.rs` that walks all variable references in expressions and statements. When `implicit_none` is active in the current scope and the referenced name is not found in the symbol table (local, USE-associated, or host-associated), emit a diagnostic error.

**Edge cases:**
- Intrinsic function names (abs, sin, etc.) should not trigger the check
- EXTERNAL declarations should suppress the check for that name
- Host association: contained procedures inherit the host's declarations
- Module procedures: the module scope's implicit rules apply

**Files:** `src/sema/validate.rs`, `src/sema/symtab.rs`

### 2. Generic Interface Resolution
**Problem:** `resolve_generic()` exists in `types.rs` but is never called. A call to a generic name (`call add(1, 2)`) emits a linker reference to `_add` instead of resolving to `_add_int` based on argument types.

**Solution:**
1. Store interface bodies (specific procedure names + their argument types) in the symbol table when parsing INTERFACE blocks
2. In `lower_expr` function-call path: when the callee symbol is `NamedInterface`, collect actual argument types and call `resolve_generic()`
3. Replace the generic name with the resolved specific procedure name before emitting the call

**Edge cases:**
- Ambiguous resolution (two specifics match) → diagnostic error
- No matching specific → diagnostic error
- Optional arguments in specifics
- Intrinsic operator interfaces (OPERATOR(+), ASSIGNMENT(=))

**Files:** `src/sema/resolve.rs`, `src/sema/symtab.rs`, `src/ir/lower.rs`, `src/sema/types.rs`

### 3. Compile-Time Kind Selection Intrinsics
**Problem:** `selected_int_kind(9)` and `selected_real_kind(15)` are recognized as intrinsics but not evaluated at compile time. When used as kind selectors (`integer(selected_int_kind(9)) :: n`), they produce the default kind because `extract_kind` can't fold the function call.

**Solution:**
1. Add `selected_int_kind(r)` to `lower_intrinsic`: return the smallest integer kind whose range covers 10^r (1→1, 2→1, 4→2, 9→4, 18→8, 38→16)
2. Add `selected_real_kind(p[, r])` similarly: return the smallest real kind with ≥p decimal digits (6→4, 15→8)
3. Add compile-time folding in `eval_const_scalar` so these work in PARAMETER and kind-selector contexts

**Files:** `src/ir/lower.rs`, `src/sema/resolve.rs`

### 4. Procedure Pointer Declarations
**Problem:** `procedure(interface_name), pointer :: var => null()` fails to parse. This syntax appears in 8 of fortsh's 55 source files, making it the single most impactful parser gap for the fortsh milestone.

**Solution:** Extend the declaration parser to recognize `PROCEDURE(name)` as a type specifier for procedure pointer declarations. Support the `pointer` attribute and `=> null()` initializer.

**Files:** `src/parser/decl.rs`, `src/ast/decl.rs`

### 5. Preprocessor `!defined()` Fix
**Problem:** `#if !defined(MACRO)` fails because the preprocessor's expression parser doesn't handle the `!` (logical NOT) operator. fortsh's `string_pool.f90` uses this.

**Solution:** Add unary `!` operator support to the preprocessor's `#if` expression evaluator.

**Files:** `src/preprocess/`

## Definition of Done
- `implicit none` + undeclared variable → compile error with clear diagnostic
- `call add(1, 2)` with generic interface resolves to correct specific procedure
- `integer(selected_int_kind(9)) :: n` produces integer(4)
- `procedure(iface), pointer :: p => null()` parses without error
- `#if !defined(X)` evaluates correctly
- fortsh module graph compiles ≥10/55 (up from 2/55)
- Zero regressions in 279 test programs
