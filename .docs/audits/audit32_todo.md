# Audit 32 Closeout Todo

Source audit: `.docs/audits/audit32.md`

This checklist turns the sprint 32 audit findings into implementation batches we
can land without mixing unrelated risks in the same patch.

Legend:

- `[ ]` not started
- `[~]` in progress
- `[x]` done

## Chunk 1: Driver / Output Correctness

Status: `[x]` first batch landed

- `[x]` Finding 2: fix multi-input `-c` semantics in `src/driver/mod.rs`
  Current direction: treat `-c` with multiple inputs and explicit `-o` as an
  error; multi-input `-c` without `-o` should not fall through to final link.
- `[x]` Finding 3: make `-E` default to stdout when `-o` is absent
- `[x]` Finding 6: skip `-no_uuid` for `-shared` outputs
- `[x]` Finding 7: emit `.amod` files for module-producing builds outside pure
  `-c`, especially `-shared`
- `[x]` Finding 9: turn `-J` write failures into hard errors
- `[x]` Finding 11: print `--time-report` even on failure paths
- `[x]` Add driver regression tests for each item above

## Chunk 2: `.amod` / ABI Correctness

Status: `[~]` core kind-materialization fix landed

- `[x]` Finding 1: always persist concrete kinds in `.amod` for default-kind
  declarations
- `[x]` Finding 13: remove implicit-kind ambiguity from the `.amod` format in
  all module variable/parameter paths
- `[x]` Add a cross-TU regression covering `-fdefault-integer-8` /
  `-fdefault-real-8` producer-consumer mismatch
- `[ ]` Confirm whether header-level default-kind stamping is still needed once
  concrete kind emission is fixed

## Chunk 3: Semantic Correctness

Status: `[x]`

- `[x]` Finding 12: make `-fimplicit-none` respect explicit `IMPLICIT` rules in
  program-unit scopes
- `[x]` Finding 24: diagnose constant integer overflow instead of saturating
- `[x]` Finding 25: diagnose compile-time integer division by zero
- `[x]` Add sema regressions for each item above

## Chunk 4: Parser / Frontend Hardening

Status: `[x]`

- `[x]` Finding 4: reject arbitrary text instead of treating it as implicit
  `CALL` statements
- `[x]` Finding 10: add an expression-depth guard so deep nesting fails
  gracefully instead of overflowing the stack
- `[x]` Finding 16: strip UTF-8 BOM before lexing
- `[x]` Finding 17: show the offending UTF-8 character or a clean escape in
  lexer diagnostics
- `[x]` Finding 18: route lexer/parser diagnostics through the caret renderer
- `[x]` Finding 19: make diagnostic gutter width dynamic for 6+ digit lines
- `[x]` Add parser/diagnostic regressions for each item above

## Chunk 5: CLI Parsing And Response Files

Status: `[x]`

- `[x]` Finding 5: fix or at least lock down `--std f2018` space-form handling
  Current: subprocess regression now confirms the space form works on the current parser path.
- `[x]` Finding 14: make `afs --version` identify as `afs`
- `[x]` Finding 15: reject or warn on duplicate `-o`
- `[x]` Findings 20-22: recursive response-file expansion with depth limit and
  quoted-token parsing
- `[x]` Finding 23: classify response-file read failures through the standard
  exit-code path
- `[x]` Finding 26: support `-I=<dir>`
- `[x]` Finding 27: support a literal-`@` escape for filenames beginning with `@`
- `[x]` Finding 28: validate `-g` suffixes instead of accepting arbitrary text
- `[x]` Finding 29: warn on unknown `-W*` flags
- `[x]` Finding 30: make `-shared` and `-static` mutually exclusive or define
  clear precedence
- `[x]` Finding 31: fix no-input help routing / exit code
- `[x]` Finding 32: support `-o=<path>` or explicitly reject it with a better
  diagnostic
- `[x]` Finding 33: define and test `--help` / `--version` precedence

## Chunk 6: Parsed-But-Ignored Flag Sweep

Status: `[x]`

- `[x]` Finding 8a: decide real semantics for `-fcheck=bounds` /
  `-fcheck=all`
- `[x]` Finding 8b: wire or explicitly no-op-warn `-fmax-stack-var-size`
- `[x]` Finding 8c: wire or explicitly no-op-warn `-frecursive`
- `[x]` Finding 8d: wire `-fbackslash` into preprocessing / lexing
  Current: explicit warning path landed; no silent acceptance remains.
- `[x]` Finding 8e: decide warning-policy behavior for `-Wall`, `-Wextra`,
  `-Wpedantic`, `-Wdeprecated`, `-Werror`
- `[x]` Finding 8f: decide whether `-g` is wired, warned, or rejected
- `[x]` Finding 8g: either implement `--diagnostics-format=json` or reject /
  warn instead of silently printing text
- `[x]` Add one regression per flag outcome so the accepted surface cannot drift

## Suggested Landing Order

1. Chunk 1: driver/output correctness
2. Chunk 2 and Chunk 3: ABI + sema correctness
3. Chunk 4: parser/diagnostic hardening
4. Chunk 5: CLI edge-case cleanup
5. Chunk 6: parsed-but-ignored flag honesty sweep
