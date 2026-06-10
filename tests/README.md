# armfortas tests

The root `tests/` directory now holds armfortas-local harnesses and integration
checks that belong in the compiler repo itself.

The extracted structured bench lives in the `bencch/` submodule.

Current handoff point:
The canonical leaf-assertion language lives in source comments inside
`test_programs/` and other shared fixtures:

- `! CHECK:`
- `! STDERR_CHECK:`
- `! EXIT_CODE:`
- `! XFAIL:`
- `! ERROR_EXPECTED:`
- `! ERROR_SPAN:`
- `! ASM_CHECK:` / `! ASM_NOT:`
- `! FILE_CHECK:` / `! FILE_NOT:`
- `! FILE_EXISTS:` / `! FILE_MISSING:`
- `! FILE_LINE_COUNT:`
- `! FILE_RERUN_MODE:`
- `! FILE_SET_EXACT:`
- `! REPRO_CHECK:`
- `! OPT_EQ:`
- `! PHASE_TRIANGULATE:`
- `! IR_CHECK:`
- `! IR_NOT:`
- `! FLAGS:` — extra compiler flags for this test, one line, whitespace-
  split, appended to every compiler invocation the harness makes for the
  test (run, IR, ASM, REPRO, OPT_EQ, helper objects). Harness-owned flags
  (`-O*`, `-o`, `-S`, `-c`, `-E`, `--emit-*`, `--target`) are rejected as
  test-configuration errors. Typical use: `! FLAGS: --std=f2023`.

That source comment language is meant to converge with `bencch`, not drift from
it. (`! FLAGS:` is not consumed by bencch yet — see noted_items.md.)

When a stdlib/fpm drill creates scratch Fortran probes, review them during the
drill wrapup. Keep the probes that capture a real compiler edge by moving them
into `test_programs/` or `tests/fixtures/` with the appropriate annotations, and
delete the rest before committing.

## Relationship To `bencch`

The extracted structured bench lives in the `bencch/` submodule. It remains a
co-equal tool, but it now has a clearer role:

- the root harness is the fast armfortas-first runner
- `bencch` is the structured matrix/reporting/differential runner

`bencch` should reuse source directives from shared fixtures whenever possible.
Its suite DSL is for orchestration — opts, references, module graphs,
capability policy, reports, and bundles — not for inventing a separate leaf
assertion language.

- `bencch` was split out after the Sprint 6 audit/hardening slice.
- The next planned bench slice is deeper Sprint 6 differential coverage and
  object/tool consistency work.
