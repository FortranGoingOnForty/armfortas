# gfortran.dg F2023 fixtures (sprint l00)

Annotated imports of the F2023 tests found in the gcc testsuite. They live
here, not in `test_programs/`, so the root harness's all-of-test_programs
sweep is unaffected until each feature lands; a dedicated runner
(`gfortran_dg_fixtures` in `tests/run_programs.rs`) drives this directory.

## Provenance

- Source: `.docs/refs/gcc/gcc/testsuite/gfortran.dg/` at gcc checkout
  `b700707a77eeaa1d37f733c4b2d2e242063c29d2 2026-06-10`
  (`git -C .docs/refs/gcc log -1 --format='%H %cs'`).
- License: the gcc testsuite is GPL-licensed and this repo is GPLv3
  (`LICENSE` at the repo root), so verbatim import is license-compatible;
  original comment headers (PR references, author attributions) are
  preserved in each file.
- Excluded: everything under `gfortran.dg/f202y/` — that is F202Y
  (next-standard) material, not F2023.

## Conversion rules

- All `{ dg-* }` directive lines are stripped. Whole-line and trailing
  `{ dg-error "..." }` markers are rewritten as plain
  `! original dg-error: "..."` comments so per-line intent survives.
- `{ dg-options "-std=f2023" }` (and friends) became a single
  `! FLAGS:` line. Only flags armfortas understands pass through;
  `-std=gnu` and dejagnu-only directives (`dg-prune-output`,
  `dg-require-effective-target`) were dropped and are noted per file.
- `{ dg-do run }` tests that use `STOP n` as failure paths: success is
  reaching the end of the program → `! EXIT_CODE: 0`. Stop codes are not
  converted into `! CHECK:` lines (`stop 1` exits 1 and prints to stderr).
- `{ dg-do compile }` positive tests on complete programs → `! EXIT_CODE: 0`.
  The two non-runnable bare modules: conditional_7 got a trivial appended
  main (noted in-file); c_f_pointer_shape_tests_8 is ERROR_EXPECTED-only,
  which needs no runnable main.
- ERROR_EXPECTED substrings must not occur verbatim in any source line
  (including the converted `! original dg-error:` trailing comments):
  armfortas diagnostics echo the offending source line, so a colliding
  substring self-matches and produces a spurious XPASS. Caught and fixed
  during import for conditional_3/4/9 (substrings re-worded or comments
  truncated).
- `{ dg-error }` negative tests: if armfortas already emits a suitable
  diagnostic, `! ERROR_EXPECTED:` carries armfortas's actual substring
  (only selected_logical_kind_2 qualifies today). Where the conformance
  gate is missing, `! ERROR_EXPECTED:` carries the diagnostic we *want*
  per the matrix row plus an `! XFAIL:`; the harness XFAIL path was
  verified by hand to catch the mismatch.
- Every fixture whose feature `.docs/audits/f2023-feature-matrix.md`
  marks missing carries
  `! XFAIL: f2023 <feature> not implemented (<sprint>); see .docs/audits/f2023-feature-matrix.md`,
  and each was hand-run once
  (`target/debug/armfortas --std=f2023 --target arm64-macos -S <file>`)
  to record the actual failure mode below.
- continuation_18.f90 was inspected before import: 267 lines / 5.8 KB of
  hand-written continuation lines (not generated bulk), so it is imported
  verbatim rather than generated on the fly.

## Host caveat (pre-x01)

This box is x86_64 FreeBSD; full compile+run hits the x00 codegen guard
("cannot generate code for target 'x86_64-freebsd': the x86_64 backend is
not implemented yet (sprint x03)"). Hand-runs therefore used
`--target arm64-macos -S`. In the runner on this host, XFAIL'd fixtures
report Xfail (the guard failure is caught by the XFAIL), and the three
fixtures with no XFAIL that need codegen FAIL here but are expected to
pass on macOS — identical in kind to the rest of the e2e suite:

- FAIL-on-host-only: `line_length_13.f90`, `continuation_18.f90`
  ("runtime verification pending (nomad batch)" — they fully compile
  under `-S` but cannot be linked/run here).
- Passes on every host: `selected_logical_kind_2.f90` (frontend-only
  rejection, matched by ERROR_EXPECTED before codegen).

## Per-file table

Hand-run = `target/debug/armfortas --std=<per FLAGS> --target arm64-macos -S <file>`
on 2026-06-10.

| file | original dg intent | converted annotations | hand-run outcome | XFAIL reason |
|---|---|---|---|---|
| conditional_1 | run, -std=f2023; STOP-based checks | FLAGS f2023; EXIT_CODE 0 | compile error: `lexer error: unexpected character: '?'` | conditional expressions missing (l02) |
| conditional_2 | run, -std=f2023 | FLAGS f2023; EXIT_CODE 0 | same lexer error on `?` | conditional expressions missing (l02) |
| conditional_3 | compile, -std=f2023; 2 dg-errors (bad syntax) | FLAGS f2023; ERROR_EXPECTED `expected ':'` | lexer error on `?` (wanted substring absent → XFAIL path verified) | conditional expressions missing (l02) |
| conditional_4 | compile, -std=f2023; 7 dg-errors (type/kind/rank) | FLAGS f2023; ERROR_EXPECTED `must be scalar` | lexer error on `?` | conditional expressions missing (l02) |
| conditional_5 | compile, -std=f2018; dg-error "Fortran 2023: Conditional expression" | FLAGS f2018; ERROR_EXPECTED `requires --std=F2023` | lexer error on `?` (gate diagnostic absent) | f2018 conformance gate for conditionals missing (l02) |
| conditional_6 | run, -std=f2023; conditional actual args | FLAGS f2023; EXIT_CODE 0 | lexer error on `?` | conditional expressions missing (l02) |
| conditional_7 | compile, -std=f2023; bare module (char-len spec) | FLAGS f2023; EXIT_CODE 0; trivial main appended | lexer error on `?` | conditional expressions missing (l02) |
| conditional_8 | run, -std=f2023; short-circuit of else-arm call | FLAGS f2023; EXIT_CODE 0 | lexer error on `?` | conditional expressions missing (l02) |
| conditional_9 | compile, -std=f2023; 3 dg-errors (index in LOCAL spec) | FLAGS f2023; ERROR_EXPECTED `LOCAL locality` | lexer error on `?` | conditionals missing (l02) + LOCAL index diagnostic missing |
| line_length_13 | compile, -std=f2023; 10,000-char limit, 2 dg-errors for 10,001/10,002 | FLAGS f2023; EXIT_CODE 0 | `-S` clean (no line limit; truncation dg-errors dropped — armfortas has no such diagnostic, l01 owns the decision) | none — FAIL-on-host-only (codegen guard); runtime verification pending (nomad batch) |
| continuation_18 | compile, -std=f2023; 255 continuations, no warning | FLAGS f2023; EXIT_CODE 0 | `-S` clean | none — FAIL-on-host-only (codegen guard); runtime verification pending (nomad batch) |
| system_clock_4 | compile, -std=f2023; 10 dg-errors (arg kind restrictions) | FLAGS f2023; ERROR_EXPECTED `SYSTEM_CLOCK` | `-S` clean — no diagnostic at all (restriction gate absent) | SYSTEM_CLOCK F2023 restrictions missing (l04) |
| selected_logical_kind_1 | run, default flags; kind probes via STOP | FLAGS f2023; EXIT_CODE 0 | compile error: `variable 'selected_logical_kind' used but not declared (IMPLICIT NONE is active)` | SELECTED_LOGICAL_KIND missing (l04) |
| selected_logical_kind_2 | compile, -std=f2018; 2 dg-errors "has no IMPLICIT type" | FLAGS f2018; ERROR_EXPECTED `used but not declared` | compile error matches the expectation — passing negative fixture, no XFAIL | — (l04 must re-word once the intrinsic + gate exist) |
| selected_logical_kind_3 | run, requires fortran_integer_16 | FLAGS f2023; EXIT_CODE 0 (effective-target dropped; armfortas has kind-16 integers) | `-S` clean but emits unresolvable `bl _selected_logical_kind` → cannot link | SELECTED_LOGICAL_KIND missing (l04); parameter-context call is laxness #2 in the matrix |
| selected_logical_kind_4 | run, default flags; non-constant context | FLAGS f2023; EXIT_CODE 0 | compile error: `'selected_logical_kind' used but not declared` | SELECTED_LOGICAL_KIND missing (l04) |
| split_1 | run, default flags; forward/backward SPLIT | FLAGS f2023; EXIT_CODE 0 | `-S` clean but emits unresolvable `bl _split` → cannot link | SPLIT missing (l04) |
| split_2 | run, default flags; UCS-4 SPLIT | FLAGS f2023; EXIT_CODE 0; XFAIL-006 | rejected because `SELECTED_CHAR_KIND('ISO_10646')` returns `-1` | nondefault character kinds unsupported |
| split_3 | run + dg-shouldfail "Fortran runtime error" (POS out of range) | FLAGS f2023; EXIT_CODE 1 (armfortas runtime-error convention, provisional until l04) | `-S` clean, unresolvable `_split` | SPLIT missing (l04) |
| split_4 | run + dg-shouldfail (BACK at string start) | FLAGS f2023; EXIT_CODE 1 (provisional) | `-S` clean, unresolvable `_split` | SPLIT missing (l04) |
| c_f_pointer_shape_tests_7 | run, -std=f2023; LOWER= honored | FLAGS f2023; EXIT_CODE 0 | `-S` clean but emits external `bl _lbound`/`bl _ubound` (matrix laxness #4) → cannot link; LOWER honoring unverified | C_F_POINTER LOWER= missing (l06) |
| c_f_pointer_shape_tests_8 | compile, -std=f2023; 2 dg-errors (LOWER type/rank) | FLAGS f2023; ERROR_EXPECTED `LOWER` (bare module, ERROR_EXPECTED-only) | `-S` clean — no validation of the LOWER argument | C_F_POINTER LOWER validation missing (l06) |
| do_concurrent_8_f2023 | compile, -std=gnu; 2 dg-errors (var in SHARED and REDUCE) | FLAGS f2023 (`-std=gnu` has no equivalent, dropped); ERROR_EXPECTED `locality-spec` | `-S` clean — duplicate-locality diagnostic absent | DO CONCURRENT REDUCE duplicate-locality diagnostic missing (l01) |

Files skipped: none of the listed l00 batch; `conditional_1.f90` through
`conditional_9.f90` all exist upstream and all were imported (23 files
total).
