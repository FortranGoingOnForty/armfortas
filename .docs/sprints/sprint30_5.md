# Sprint 30.5: Test Bench Enhancement & Full-Spectrum Verification

## Context

Sprint 30 delivered multi-file compilation via .amod module files. Four brutal audit rounds across sprints 29–30 surfaced 50+ bugs — every single one in an area with zero or insufficient test coverage. The existing harness (198 single-file test programs, 40 Rust test harnesses, 20 annotation types) is impressive for single-file correctness but has critical blind spots: no cross-TU CI coverage until `tests/multifile.rs` was added at the end of 30, no fuzzing, no incremental compilation tests, no systematic negative tests, no ABI compatibility matrix, no coverage measurement, and no performance regression tracking. Sprint 30.5 fills these gaps so that sprints 31+ can build on a test foundation that catches bugs before auditors do.

## Prerequisites
Sprint 30 (module system), Sprint 29 (optimization pipeline)

## Goals
Transform the test bench from "catches bugs we already know about" to "prevents bugs we haven't thought of." Every new feature should be impossible to ship without the harness catching regressions. The test bench itself should be a showcase of armfortas's engineering quality — as impressive as the compiler.

## Deliverables

### 1. Declarative Multi-File Test Format

**Problem:** `tests/multifile.rs` hardcodes source strings in Rust. Adding a new cross-TU test requires writing Rust code. This is friction that discourages coverage.

**Solution:** A new annotation-driven format where multiple files live in a single `.f90` with separator markers:

```fortran
!--- module: mymod.f90
module mymod
  implicit none
  integer :: x = 42
end module
!--- program: main.f90
program p
  use mymod
  print *, x
end program
! CHECK: 42
! OPT_EQ: O0,O1,O2 => stdout|exit
! MULTIFILE_LINK: mymod.f90 main.f90
```

The harness splits on `!--- module:` / `!--- program:` markers, compiles each segment to its own `.o`, links per `MULTIFILE_LINK`, runs the binary, and applies standard CHECK/OPT_EQ/XFAIL annotations.

**Files:** `test_programs/multifile_*.f90` (new), `tests/run_programs.rs` (extend parser)

### 2. Dependency Chain Generator

**Problem:** Sprint 30's module system was tested with 1–2 level dependency chains. Real Fortran projects (fortsh: 55 modules) have deep, branching dependency graphs.

**Solution:** A Rust test generator that programmatically builds N-module chains and diamond patterns:

```
gen_chain(depth=10)     → M1 uses M2, M2 uses M3, ..., M10 uses nothing
gen_diamond(width=4)    → A uses B1..B4, each Bi uses C
gen_tree(depth=3, fan=3) → tree-shaped dependency graph
```

Each generated scenario compiles with automatic ordering, links, and verifies that module N can access symbols from module 1 through the chain.

**Files:** `tests/multifile_gen.rs` (new)

### 3. Cross-Optimization-Level ABI Matrix

**Problem:** We test that O0 and O2 produce the same output (OPT_EQ), but never test that O0-compiled and O2-compiled object files can be LINKED together. ABI drift between opt levels would silently corrupt cross-TU calls.

**Solution:** For each `test_programs/*.f90` that has `OPT_EQ`, compile module at O0 + consumer at O2, and vice versa. Link both combinations. Run both. Output must match.

```
! ABI_MATRIX: O0,O2
```

New annotation. The harness generates the cross-level link combinations automatically.

**Files:** `tests/run_programs.rs` (extend), `tests/multifile.rs` (extend)

### 4. Systematic Negative Test Suite

**Problem:** Only 10 test programs use `ERROR_EXPECTED`. The Fortran standard defines hundreds of constraints that must produce compile-time errors. Our sema validates many of them but has no test coverage for most.

**Solution:** Create `test_programs/error_*.f90` programs for every diagnosticable constraint:

- `error_intent_in_write.f90` — write to INTENT(IN) dummy
- `error_pure_io.f90` — I/O in PURE procedure
- `error_pure_stop.f90` — STOP in PURE
- `error_pure_impure_call.f90` — PURE calls non-PURE
- `error_pure_nonlocal_write.f90` — PURE writes module variable
- `error_pointer_nonpointer.f90` — pointer assignment to non-pointer
- `error_pointer_nontarget.f90` — pointer to non-TARGET
- `error_allocatable_parameter.f90` — ALLOCATABLE + PARAMETER
- `error_use_nonexistent.f90` — USE of missing module
- `error_duplicate_label.f90` — duplicate statement labels
- `error_goto_undefined.f90` — GOTO to undefined label
- ... (target: 50+ negative tests)

Each uses `ERROR_EXPECTED` + `ERROR_SPAN` annotations. The harness rejects false positives (compiling when it shouldn't) and false negatives (wrong error message).

**Files:** `test_programs/error_*.f90` (50+ new)

### 5. Incremental Compilation Test Suite

**Problem:** Sprint 30 built .amod for module caching but has no automated tests for incremental rebuild correctness — stale .amod detection, dependency-chain invalidation, or unnecessary recompilation.

**Solution:** Tests in `tests/incremental.rs` that:
1. Compile a module chain (A → B → C)
2. Touch B's source, recompile — verify C is NOT recompiled (its .amod didn't change)
3. Change B's public interface, recompile — verify C IS recompiled
4. Change B's private implementation only — verify C is NOT recompiled
5. Verify .amod checksum-based staleness warnings

**Files:** `tests/incremental.rs` (new)

### 6. Circular Dependency Detection Tests

**Problem:** `USE A` in B + `USE B` in A should produce a clear error. Currently untested.

**Solution:** Negative tests for:
- Direct cycle: A uses B, B uses A
- Indirect cycle: A uses B, B uses C, C uses A
- Self-use: A uses A

Each should produce a diagnostic, not hang or crash.

**Files:** `test_programs/error_circular_use.f90`, `tests/multifile.rs` (extend)

### 7. Fuzzing Harness

**Problem:** Grammar edge cases, malformed input, and unexpected token sequences are never tested. Every lexer/parser bug found in audits was from code the harness didn't exercise.

**Solution:** Two fuzzing targets:
1. **Lexer fuzzer** — feeds random bytes to `Lexer::tokenize`, asserts no panics
2. **Parser fuzzer** — feeds random token streams to `Parser::parse_file`, asserts no panics
3. **Roundtrip fuzzer** — compiles random syntactically-valid Fortran at O0 and O2, asserts output matches (differential)

Integrate with `cargo fuzz` (libfuzzer).

**Files:** `fuzz/fuzz_targets/fuzz_lexer.rs`, `fuzz/fuzz_targets/fuzz_parser.rs`, `fuzz/fuzz_targets/fuzz_roundtrip.rs` (new)

### 8. Coverage Measurement Infrastructure

**Problem:** No way to identify dead code paths in the compiler or runtime. No data-driven gap analysis.

**Solution:**
- `cargo tarpaulin` or `cargo llvm-cov` integration
- CI step that generates coverage report after `cargo test`
- Target: identify untested branches in `src/ir/lower.rs`, `src/opt/*.rs`, `src/codegen/*.rs`
- Coverage badge in README

**Files:** `.github/workflows/ci.yml` (extend), `scripts/coverage.sh` (new)

### 9. Performance Regression CI

**Problem:** The `bencch/` benchmark infrastructure exists but is NOT in CI. A 10x compile-time regression could ship unnoticed.

**Solution:**
- CI step that runs a small benchmark suite (5 representative programs) and records compile time + binary size
- Fail CI if compile time regresses by >20% or binary size by >10%
- Store historical baselines in `.benchmarks/` (committed)

**Files:** `.github/workflows/ci.yml` (extend), `scripts/benchmark_gate.sh` (new)

### 10. Determinism Audit Automation

**Problem:** `REPRO_CHECK` exists on 40 tests but is opt-in. A new test program with no `REPRO_CHECK` annotation can have non-deterministic codegen and nobody notices.

**Solution:** A CI step that compiles ALL `test_programs/*.f90` twice at O2 and `cmp`s the .s output. Any diff is a failure.

**Files:** `tests/determinism_sweep.rs` (new)

### 11. Sanitizer Integration

**Problem:** No ASAN/MSAN on compiled Fortran programs. Memory bugs in the runtime manifest as silent corruption.

**Solution:**
- Compile `libarmfortas_rt` with ASAN instrumentation
- Run a subset of test programs linked against the ASAN'd runtime
- Separate CI job (slower, not on every push — triggered on PR or weekly)

**Files:** `.github/workflows/sanitizers.yml` (new), `scripts/asan_runtime.sh` (new)

### 12. fortsh Module Graph Smoke Test

**Problem:** Sprint 30 spec called for "fortsh module graph resolves correctly." This is the ultimate integration test — 55 modules with real dependency chains.

**Solution:** A test that:
1. Scans fortsh source for `module` and `use` statements
2. Builds the dependency graph
3. Compiles in topological order (each .f90 → .o + .amod)
4. Verifies no compile errors (not full linking — that's sprint 33)

This catches module system regressions against real-world code without requiring fortsh to fully compile.

**Files:** `tests/fortsh_module_graph.rs` (new)

## Testing Strategy (Meta)

Every deliverable in this sprint adds test infrastructure, not test content. The goal is mechanisms that generate coverage, not hand-written tests that cover specific bugs. The audit-fix-audit cycle from sprints 29–30 should become unnecessary when the test bench is rich enough to prevent regressions proactively.

## Definition of Done
- Declarative multi-file tests work with `!--- module:` separators in run_programs harness
- ≥3 generated dependency chain tests pass (depth 5, 10, diamond)
- ≥50 ERROR_EXPECTED negative tests covering sema diagnostics
- Incremental compilation test suite passes
- Circular USE detection tested and passing
- Fuzzing targets exist and run locally (not necessarily in CI yet)
- Coverage report generates successfully
- Determinism sweep passes on all 198+ test programs
- fortsh module graph compiles without errors
