# Sprint 33 Readiness Audit

Date: 2026-04-15
Branch: `trunk`
Compiler build used: `target/debug/armfortas`

## Executive Summary

Sprint 33 assumes "all prior sprints (the compiler is feature-complete)" and
that fortsh work can begin in earnest. The local closeout work for this audit
resolved the blockers below.

I validated these findings against the sprint docs, `.docs/noted_issues.md`,
the current tests, and direct compiler runs.

## Key Evidence

- Sprint 31 promises fixed-form end-to-end and per-standard suites.
- Sprint 31.1 promises a default standard of F2018 and broad feature gating.
- Sprint 31.5 promises coarray syntax stubs and legacy robustness work.
- Sprint 33 assumes prior sprint completion and compiler feature-completeness.

Current reality:

- A canonical `.f` hello-world still tokenizes `PROGRAM HELLO` as
  `PROGRAMHELLO` and fails in parsing.
- `--std=f95` still accepts `impure`, `submodule`, `abstract type`,
  `class(*)`, `type(*)`, deferred-length character, allocatable scalars, and
  `allocate(..., source=...)`.
- `-Wpedantic` / `-Wdeprecated` only warn that the warning groups are not yet
  implemented.
- `ENTRY` is still unimplemented.
- Coarray syntax still fails in parsing instead of reaching a clear
  "not implemented" diagnostic.
- `cargo test --test fortsh_module_graph -- --nocapture` now gates on a real
  compiled floor (`14/55`), uses the freshly built debug compiler in test
  runs, and fails on hard compiler failures (ICEs / assembler crashes) instead
  of treating fortsh as informational-only.

## Findings

### 1. P1: Fixed-form is not end-to-end viable

- Area: `src/lexer/fixed.rs`, `tests/run_programs.rs`
- Symptom: whitespace-stripped fixed-form letter runs are emitted as whole
  identifiers, so `PROGRAM HELLO` becomes `PROGRAMHELLO`.
- Impact: sprint 31's fixed-form pipeline claim does not currently hold.
- Test gap: the main `run_programs` harness only discovers `.f90`, so `.f`
  regressions are invisible to the core runner.
- Status: resolved locally on 2026-04-15.

### 2. P1: `--std=` gating is far narrower than claimed

- Area: `src/driver/mod.rs`, `src/sema/validate.rs`, parser feature sites
- Symptom: only `ERROR STOP`, `DO CONCURRENT`, `BLOCK`, and `ASSOCIATE` are
  currently gated, and only when `--std` is explicitly set.
- Impact: sprint 31.1's standards matrix is not enforced, and the documented
  default of F2018 is not active.
- Status: resolved locally on 2026-04-15.

### 3. P2: Pedantic / deprecated warnings are still placeholders

- Area: `src/driver/mod.rs`
- Symptom: `-Wpedantic` and `-Wdeprecated` emit only "recognized but not yet
  implemented" warnings.
- Impact: the CLI advertises standard warning groups that are not yet wired to
  any real conformance or deprecation diagnostics.
- Status: resolved locally on 2026-04-15.

### 4. P2: Remaining sprint 31.x legacy / robustness gaps

- Area: parser / statement handling
- Symptom: `ENTRY` remains unimplemented; coarray syntax still parse-errors
  instead of producing a clear unsupported-feature diagnostic.
- Impact: sprint 31.5 is not fully closed, and some F77 / F2008 edges remain
  below the claimed bar.
- Status: resolved locally on 2026-04-15.

### 5. P2: fortsh smoke test is not yet a readiness gate

- Area: `tests/fortsh_module_graph.rs`
- Symptom: the test passes with only one successfully compiled fortsh file.
- Impact: sprint 33 readiness is overstated if this test is treated as a pass
  signal. The current score is informational, not gating.
- Status: resolved locally on 2026-04-15. The gate now records a `14/55`
  compiled floor and rejects hard compiler failures while leaving honest
  unsupported-feature diagnostics visible in the scorecard.

## Closeout Order

1. [x] Fix fixed-form token boundaries and add real `.f` coverage.
2. [x] Expand `--std=` enforcement and make the default standard explicit.
3. [x] Replace pedantic placeholders with real diagnostics or clear rejections.
4. [x] Close the remaining legacy / robustness gaps (`ENTRY`, coarray stubs).
5. [x] Raise the fortsh smoke-test floor and use it to drive sprint 33 fixes.
