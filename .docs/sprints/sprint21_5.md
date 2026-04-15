# Sprint 21.5: Deferred Item Cleanup

## Prerequisites
Sprint 21 (codegen complete, 16 test programs passing)

## Goals
Fix the 14 category-A deferred items that block forward progress into Phase 7 (Runtime Library). These are correctness bugs that will cascade the moment we touch strings, allocatables, non-integer types, or multi-module programs. Split into two batches: parser/sema fixes and IR lowering fixes.

## Batch 1: Parser & Semantic Analysis

### 1. Blank lines consumed by continuation scanning
**Source:** Lexer (Sprint 5-6)
**Problem:** When a blank line follows a fixed-form statement, the continuation-scanning while-loop consumes it instead of emitting `FixedLine::Blank`. Multi-line Fortran files can silently misparse.
**Fix:** Break the continuation scan on blank lines.
**Test:** Fixed-form source with blank lines between statements parses correctly.

### 2. Wire `parse_derived_type_def` into `parse_unit_body` dispatcher
**Source:** Parser (Sprint 8)
**Problem:** Derived type definitions inside modules/programs (`type :: mytype ... end type`) aren't recognized by the body parser. Blocks Sprint 28 (Derived Types & OOP) and any test program using derived types.
**Fix:** Add `type` keyword detection in `parse_unit_body` Phase 3 to dispatch to `parse_derived_type_def`. (Note: partial detection exists for `type ::` and `type ,` — verify it works end-to-end.)
**Test:** Module with a derived type definition parses without error.

### 3. Module scope re-entry — add `enter_scope(id)`
**Source:** Sema (Sprint 12)
**Problem:** `resolve.rs` re-enters pre-created module scopes by mutating `parent` pointer — fragile, breaks with nested modules or multi-pass resolution.
**Fix:** Add `SymbolTable::enter_scope(id: ScopeId)` method that sets `current = id` without creating a new scope. Use it in `resolve_unit` for modules.
**Test:** Multi-module file resolves correctly with proper scope isolation.

### 4. `type_spec_to_info` propagates kind selectors
**Source:** Sema (Sprint 12)
**Problem:** `integer(8)` becomes `TypeInfo::Integer { kind: None }`. Kind info is discarded, producing wrong sizes in type checking and IR lowering.
**Fix:** Extract kind value from `KindSelector::Expr` (integer literals) and `KindSelector::Star` in `type_spec_to_info`. Store in `TypeInfo::Integer { kind: Some(8) }`.
**Test:** `integer(8) :: x` resolves to `TypeInfo::Integer { kind: Some(8) }`.

### 5. Function return_type propagation
**Source:** Sema (Sprint 12)
**Problem:** Function symbols in the symbol table have no type_info for their return type. Callers can't determine whether a function returns integer, real, etc.
**Fix:** In `resolve_unit` for `ProgramUnit::Function`, set the function symbol's `type_info` from `return_type` or the prefix type specifier. Register the function in the parent scope with this info.
**Test:** `y = myfunc(x)` resolves `myfunc` with correct return type.

### 6. `consume_end` trailing identifier boundary check
**Source:** Parser (Sprint 11)
**Problem:** After `end do`, any following identifier is consumed as a potential construct name without checking for a newline boundary. Can silently eat the next statement's first token.
**Fix:** Only consume the trailing identifier if it's on the same line as `end`.
**Test:** `end do\nfoo = 1` parses `foo = 1` as a separate assignment, not part of `end do`.

### 7. Implicit conversion insertion in IR lowering
**Source:** Sema (Sprint 13)
**Problem:** Mixed-type expressions (e.g., `integer + real`) lower to IR without conversion instructions. The integer operand is used directly with a float instruction, producing wrong results or type mismatches.
**Fix:** In `lower_expr` for `BinaryOp`, check operand types. When they differ (e.g., one int, one float), insert `IntToFloat` or `FloatToInt` conversion before the operation. Use `sema::types::arithmetic_result_type` to determine the target type.
**Test:** `x = 1 + 2.0` produces IR with `int_to_float` before `fadd`. Compile and run a mixed-type arithmetic program.

### 8. Generic resolution with type coercion
**Source:** Sema (Sprint 13)
**Problem:** `resolve_generic` requires exact type equality. Overloaded interfaces with implicit-convertible arguments (e.g., integer arg matching a real dummy) fail to resolve.
**Fix:** In `is_specific_match`, allow numeric type promotions (integer → real → complex) when checking actual vs dummy types, not just exact equality.
**Test:** Generic interface with `integer` and `real` specifics resolves correctly when called with an integer argument.

## Batch 2: IR Lowering

### 9. ALLOCATE passes shape arguments to runtime
**Source:** IR (Sprint 16)
**Problem:** `RuntimeFunc::Allocate` called with zero arguments. No size is computed from the allocation shape subscripts. Every ALLOCATE produces a zero-sized buffer.
**Fix:** In `lower_stmt` for `Stmt::Allocate`, evaluate the shape subscripts, compute `total_elements * elem_size`, and pass the byte count to the runtime call.
**Test:** `allocate(a(100))` passes 400 (100 * 4 bytes for integer) to `afs_allocate`.

### 10. Params typed from declarations
**Source:** IR (Sprint 16)
**Problem:** All subroutine/function params hardcoded as `Ptr(I32)` regardless of declared type. A `real(8)` argument gets the wrong pointer type.
**Fix:** In `lower_unit` for Subroutine/Function, look up each dummy argument's type from the declaration list and use the correct `IrType`. Fall back to `Ptr(I32)` only if no declaration found.
**Test:** Subroutine with `real :: x` parameter gets `Ptr(Float(F32))` in IR.

### 11. Function call return types from symbol table
**Source:** IR (Sprint 16)
**Problem:** `lower_expr` for `FunctionCall` hardcodes `IrType::Int(IntWidth::I32)` as the return type. Any function returning real, logical, or character gets the wrong type.
**Fix:** Look up the callee name in the symbol table. If it's a known function with return type info, use that type. Fall back to `I32` only for unresolved externals.
**Test:** `y = real_func(x)` where `real_func` returns `real` produces `call @real_func(...) : f32`.

### 12. Runtime-variable negative DO step
**Source:** IR (Sprint 16)
**Problem:** Constant negative steps correctly use `>=` comparison, but runtime-determined step direction falls back to `<=`. A DO loop with step computed at runtime may loop forever.
**Fix:** When the step is not a compile-time constant, emit a runtime sign check: if `step < 0` use `>=`, else use `<=`. This requires a conditional branch before the loop check.
**Test:** `do i = n, 1, step` where `step = -1` executes the correct number of iterations.

### 13. ASSOCIATE scope save/restore
**Source:** IR (Sprint 16)
**Problem:** Associate names are inserted into `ctx.locals` but never removed after `END ASSOCIATE`. Names leak into the enclosing scope.
**Fix:** In `lower_stmt` for `Stmt::Associate`, save the current locals keys before the block, restore after. Or use a scope stack approach — push/pop a local scope.
**Test:** Variable `n` from `associate(n => expr)` is not accessible after `end associate`.

### 14. Integer literal kind-aware emission
**Source:** IR (Sprint 16)
**Problem:** All integer literals cast to `i32` via `val as i32`. Literals exceeding 32-bit range (e.g., `integer(8) :: big = 9999999999`) are silently truncated.
**Fix:** In `lower_expr` for `IntegerLiteral`, check the kind suffix. If kind=8 or the value exceeds i32 range, emit `const_i64`. In codegen, `emit_const_int` already handles 64-bit values correctly.
**Test:** `integer(8) :: x = 9999999999` compiles and prints the correct value.

## Testing Strategy

### Regression
All 16 existing test programs must continue to produce identical output. Run the full `cargo test --test run_programs` harness after each batch.

### New test programs
Add at minimum:
- `test_programs/mixed_types.f90` — integer + real arithmetic, verify correct result
- `test_programs/real_function.f90` — function returning real, called from program
- `test_programs/large_integer.f90` — integer(8) literals and arithmetic

### Unit tests
Each fix should include targeted unit tests in the relevant module's `#[cfg(test)]` block.

## Definition of Done
- All 14 items fixed with tests
- All 16 existing test programs still pass
- At least 3 new test programs exercising mixed types, real returns, and large integers
- `cargo test --workspace` green
- `cargo clippy --workspace` clean
- Ready to begin Sprint 22 (Runtime: Memory Management & Descriptors)
