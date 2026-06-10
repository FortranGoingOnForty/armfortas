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

Target qualifiers (sprint x01): `XFAIL`, `ASM_CHECK`, and `ASM_NOT`
accept an optional parenthesized selector list before the colon —
`! XFAIL(x86_64-linux): reason`, `! ASM_CHECK(arm64): ldp x29, x30`.
The directive is active iff any selector matches the target the harness
is compiling for (the host, until x07). Bare forms keep their
all-targets meaning. Selectors are a closed set reusing the `--target`
triple grammar plus shorthands: `arm64`/`aarch64`, `x86_64`/`amd64`,
`macos`, `freebsd`, `linux`, `x86_64-linux` (both libcs), or a full
triple. An inactive qualified `XFAIL` behaves as if absent — the
program must pass. Unknown selectors are test-configuration errors on
every target: a typo'd selector silently matching nothing would convert
a tracked bug into a green test.

Skip discipline (sprint x01): suites that assemble, link, or run native
binaries call `armfortas::testing::native_e2e_support()` before
compiling anything and, on hosts without the native toolchain, print
one machine-readable line per `#[test]` —
`HARNESS_SKIP suite=<s> test=<t> count=<n> reason="..."` — with `count`
computed from discovery, never a literal. `ci/check_skips.sh` gates CI
on these lines: zero allowed on macOS, exactly the expected set on ELF
hosts.

That source comment language is meant to converge with `bencch`, not drift from
it. (`! FLAGS:` and target qualifiers are not consumed by bencch yet —
see noted_items.md.)

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
