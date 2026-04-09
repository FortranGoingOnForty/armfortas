# armfortas tests

The root `tests/` directory is the armfortas-first testing lab.

This is where new source-directed testing ideas should land first:

- end-to-end runtime assertions
- expected diagnostics
- IR and later ASM shape assertions
- determinism and full-pipeline regression checks

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

## Roadmap

The testing reset and follow-through sprints now live under
`.docs/testing/`. Those docs define:

- the armfortas-first doctrine
- the shared annotation roadmap
- pipeline oracle work
- metamorphic and generated testing
- determinism, reduction, and triage
- fortsh-scale testing campaigns
