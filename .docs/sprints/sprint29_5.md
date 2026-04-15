# Sprint 29.5: Performance & Cleanup

## Prerequisites
Sprint 29 (Optimization Passes)

## Goals
Address accumulated performance issues and code quality items that were deferred as non-blocking.

## Deliverables

### 1. `value_type()` O(n) → HashMap Cache
`Function::value_type()` currently walks all params, block params, and instructions for every lookup. Add a `HashMap<ValueId, IrType>` cache populated during construction. Critical for large functions (fortsh's 8800-line readline).

### 2. `AcValue::ImpliedDo` Boxing
Box the `ImpliedDo` variant to reduce the `AcValue` enum from 288 bytes to ~8 bytes. Reduces memory pressure for array constructor parsing.

### 3. Preprocessor `is_emitting()` Counter
Replace the O(n) condition stack iteration with a running counter. Increment on `#if`/`#ifdef` true, decrement on `#endif`. O(1) instead of O(nesting_depth).

### 4. Preprocessor Codepath Unification
Merge `expand_condition_macros` and `expand_macros_inner` into a single expansion engine. Eliminates the dual-codepath maintenance burden.

### 5. I128 for integer(16)
Add `IntWidth::I128` and `IrType::Int(I128)`. Wire into kind mapping, type promotion, and codegen. ARM64 needs register pair handling for 128-bit ops.

## Definition of Done
- value_type() benchmark shows O(1) lookup
- All existing tests pass
- No functional regressions
