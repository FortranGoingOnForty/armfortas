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
- `! REPRO_CHECK:`
- `! OPT_EQ:`
- `! PHASE_TRIANGULATE:`
- `! IR_CHECK:`
- `! IR_NOT:`

That source comment language is meant to converge with `bencch`, not drift from
it.

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
