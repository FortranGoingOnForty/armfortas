# Audit 13 Closeout Ledger

Source audit: `.docs/audits/audit-13.md`

This ledger covers every confirmed finding, Low issue, policy decision,
coverage gap, and plausible observation from Audit 13. Items may be reordered
when a dependency makes that safer, but none may disappear without a linked
fix, verification, or explicit refutation.

Legend:

- `[ ]` not started
- `[~]` in progress
- `[x]` fixed and regression-tested, or explicitly resolved as documented

## Tranche 0: Preserve the audit baseline

Status: `[x]`

- `[x]` Record the baseline-pinned Audit 13 report.
- `[x]` Record all confirmed, Low, policy, coverage, and plausible items in
  this closeout ledger.
- `[x]` Commit the audit and ledger without production-source changes.

## Tranche 1: Lexical cleanup on loop control edges

Status: `[x]`

- `[x]` A13-01: retain the owning lexical-cleanup depth in every loop scope.
- `[x]` Route named and unnamed `EXIT` and `CYCLE` through cleanup-aware edges.
- `[x]` Test finalization and allocatable cleanup across one and multiple
  nested `BLOCK` constructs at O0 and O3.
- `[x]` Confirm ordinary loop branches with no crossed cleanup remain stable.

## Tranche 2: Fail-closed parser boundaries

Status: `[x]`

- `[x]` A13-04: restrict explicit `CALL` to procedure-designator forms.
- `[x]` Retain semantic and lowering invariant checks for invalid call shapes.
- `[x]` A13-05: diagnose unrecognized derived-type component/binding lines and
  invalid attributes before recovery.
- `[x]` Low L05: reject statement labels outside the Fortran five-digit range.
- `[x]` Assert diagnostics, exit status, and no output publication for each
  invalid-source family.

## Tranche 3: One truthful processor-kind model

Status: `[x]`

- `[x]` A13-02: share one complete `SELECTED_INT_KIND` /
  `SELECTED_REAL_KIND` algorithm between constant and runtime paths.
- `[x]` Preserve optional/keyword positions, including absent `P`, `R`, and
  `RADIX`, reordered keywords, and all defined negative results.
- `[x]` A13-09: make `*_KINDS` rank-one arrays populated from the same model.
- `[x]` Make `REAL128` negative while kind 16 is unsupported.
- `[x]` A13-03: lower aggregate `C_SIZEOF` from semantic interoperable layout
  before pointer decay.
- `[x]` Test scalar/array kinds, padded `BIND(C)` records, runtime extents, and
  literal/runtime equivalence at O0 and O3.

## Tranche 4: Central OpenMP nesting model

Status: `[x]`

- `[x]` A13-06: reject `BARRIER` closely nested in `SINGLE` and `ORDERED`.
- `[x]` A13-07: reject named, case-variant, unnamed, and non-closely nested
  same-name `CRITICAL` regions.
- `[x]` A13-14: reject worksharing `DO`/`LOOP` within `SINGLE` and other
  prohibited worksharing nests.
- `[x]` Replace independent string deny-lists with structured region frames
  and a shared OpenMP nesting-relation model.
- `[x]` Low L06: align `_OPENMP`/`openmp_version` claims with the documented
  supported subset, or explicitly define and test the preview policy.
- `[x]` Add compile-fail tests so prohibited forms never need runtime timeout
  tests in the ordinary suite.

## Tranche 5: OpenMP initial team and collapsed-loop scaling

Status: `[x]`

- `[x]` A13-15: represent the implicit initial team consistently for scalar,
  array, real, complex, blocking, and `NOWAIT` reductions.
- `[x]` Cover standalone reduction loops outside explicit parallel regions at
  O0 and O3, including empty loops.
- `[x]` A13-16: replace linear-per-iteration triangular index reconstruction
  with checked constant- or logarithmic-time inversion.
- `[x]` Verify first/middle/last indices, both stride signs, empty rows, and
  near-limit arithmetic with operation-count tests rather than timing.

## Tranche 6: Separate-compilation and nominal-type truth

Status: `[x]`

- `[x]` A13-08: serialize and import module-variable `ASYNCHRONOUS` and
  `CONTIGUOUS` attributes.
- `[x]` Bump the exact `.amod` version and add stale-artifact diagnostics.
- `[x]` Compare combined and split compilation for both attributes and the
  OpenMP asynchronous safety gate.
- `[x]` A13-10: preserve distinct IEEE nominal derived types through semantic
  checking and lower them to integer storage only afterward.
- `[x]` Low/U03: decide and test stricter rejection of self-consistent but
  invalid `.amod` enum/interface records.

## Tranche 7: Runtime I/O and driver lifecycle

Status: `[x]`

- `[x]` A13-11: reject invalid `OPEN` enumerated values before file/unit state
  changes and report through `IOSTAT`/`IOMSG`.
- `[x]` A13-12: implement common list-directed repeat counts, null values, and
  successful slash termination for every typed reader.
- `[x]` A13-13: make an explicit invalid `AFS_RUNTIME_PATH` fail closed.
- `[x]` Low L01: define and consistently implement the empty
  `EXECUTE_COMMAND_LINE` policy.
- `[x]` Low L02: emit `--time-report` for successful token/AST/IR terminal
  modes.
- `[x]` Low L03: use the documented linker exit category when linker launch
  fails.
- `[x]` Low L04: give multi-input temporary directories ownership guards and
  clean partial objects on failure.
- `[x]` Historical residual R01: narrow broad `-g*` parsing so unsupported
  suffixes cannot become only a suppressible generic warning under `-w`.

## Tranche 8: ELF group and extended-index correctness

Status: `[x]`

- `[x]` A13-21: parse and validate ELF groups and select COMDAT groups before
  ordinary section/symbol resolution.
- `[x]` Carry losing-group removal atomically through archives, relocations,
  init/fini arrays, GC, static output, and dynamic output.
- `[x]` A13-22: decode extended section count/string-table index in afs-ld
  before table validation.
- `[x]` A13-23: add `SHT_SYMTAB_SHNDX` production and consumption for
  high-index symbol definitions.
- `[x]` Add direct afs-as to afs-ld fixtures and reference-linker comparisons.

## Tranche 9: Mach-O relocation and publication contracts

Status: `[x]`

- `[x]` A13-24: retain and validate complete ARM64 ADDEND/SUBTRACTOR prefix
  metadata before relocation fusion.
- `[x]` Replace the invalid `ADDEND + UNSIGNED` success test with rejection and
  output-nonpublication coverage.
- `[x]` Low L12: preserve valid noncanonical dylib/RPATH load-command bytes or
  explicitly narrow the public reader/writer contract.
- `[x]` Policy P01: treat executable plus requested link map as one
  transaction; stage both, roll back commit failures, and test the decision.

## Tranche 10: Exact test-debt and platform coverage identities

Status: `[x]`

- `[x]` A13-17: represent harness failures by stage/kind and bind each XFAIL
  ID to an expected failure signature.
- `[x]` A13-18: require successful `-S` for every ordinary x86 differential
  cell and compare an exact expected identity set.
- `[x]` A13-19: give afs-as and afs-ld exact per-platform skip manifests.
- `[x]` Coverage G01: add opt-in exact stdout/stderr/diagnostic directives and
  migrate tests whose complete stream is the contract.
- `[x]` Coverage G02: choose and enforce a CI tier for the 55 bencch suites and
  inventory the nested future fixtures.

Closure evidence:

- XFAILs now carry structural stage/kind/signature contracts, and the x86
  differential owns an exact 891-source identity manifest covering 2,298
  ordinary O0/O2/O3 comparison cells plus 125 diagnostic sources.
- afs-as exact platform manifests account for 40 macOS-arm64 and 875
  Linux-x86_64 skip identities; afs-ld accounts for 57 macOS and 132 Linux
  identities. Both complete standalone suites passed while establishing the
  baselines.
- `STDOUT_EXACT`, `STDERR_EXACT`, and `DIAGNOSTIC_EXACT` preserve the legacy
  substring contract while allowing complete escaped-stream assertions.
- CI now runs a native bencch O0 tier of 196 cells (31 frontend/stage/graph,
  162 consistency-runtime, and 3 nested future-I/O cells). Exact manifests
  account for all 55 authored suites, all 358 cases, and every nested
  `test_programs` source plus its bencch owners.
- Opening the bencch tier retired four stale XPASS expectations, aligned
  check-comment whitespace semantics with the root harness, refreshed stage
  capture assertions, and fixed the misleading missing-`END SELECT` parser
  diagnostic it exposed.

## Tranche 11: Release and benchmark fail-closed behavior

Status: `[x]`

- `[x]` A13-20: package committed root/submodule blobs or reject tracked dirty
  state before constructing a release archive.
- `[x]` Low L07: make benchmark comparison require an existing exact baseline;
  reserve baseline creation for an explicit update mode.
- `[x]` Low L08: make the release smoke compare normalized complete output and
  require empty stderr.
- `[x]` Low L09: content-hash every pinned official OpenMP source.
- `[x]` Low L10: quote and guard benchmark temporary cleanup correctly.
- `[x]` Low L11: correct native-vs-ELF afs-as routing documentation, or change
  routing and prove the new default in a subprocess test.

Closure evidence:

- The source packager exports the root `HEAD` and every pinned submodule commit
  from Git's object store. Its regression dirties and stages different root and
  submodule bytes, then verifies that only the committed blobs enter the
  deterministic archive.
- Benchmark comparison now refuses a missing baseline, validates the complete
  ordered identity set and row schema, and reserves creation for `--update`.
  Its guarded, quoted cleanup owns only a `mktemp` directory beneath the
  selected temporary root; the regression exercises a root containing spaces.
- Release smoke validation accepts exactly one normalized `42` record and an
  empty stderr stream. The checker is required in the packaged archive, and
  focused tests reject substring-only matches, suffixes, extra/blank records,
  and stderr diagnostics.
- All 15 pinned OpenMP Examples sources now carry and verify SHA-256 identities,
  matching the existing content-hash discipline for the pinned OpenMP V&V
  sources.
- README assembler claims now match the driver: native Apple-Silicon Mach-O
  uses Apple `as`, x86_64 ELF uses in-process `afs-as`, and cross-target Mach-O
  object generation uses embedded `afs-as`; a documentation contract test pins
  those distinctions.

## Tranche 12: Convert plausible observations into decisions

Status: `[x]`

- `[x]` U01: establish or refute source reachability for overlapping-byte
  alias answers in dead-store elimination; fix and test if reachable.
- `[x]` U02: establish or refute loop-interchange reachability and semantic
  transformation; fix or remove/disable the pass if its contract is false.
- `[x]` U03 is tracked with `.amod` strictness in Tranche 6.
- `[x]` Record every refutation with a complete source trace and durable test
  where regression risk remains.

Closure evidence:

- U01 is a real IR-boundary defect. Source lowering deliberately uses
  byte-addressed GEP cursors with typed scalar accesses, while DSE compared an
  intervening load only at its starting address. A load of bytes `[4, 8)` was
  therefore treated as disjoint from a pending 8-byte store at `[0, 8)` and a
  later exact overwrite could erase the observed store. Alias analysis now
  compares checked byte ranges at the DSE memory-access boundary; regressions
  cover both overlapping and adjacent disjoint ranges without weakening exact
  overwrite elimination.
- U02's implementation did not rewrite the loop-nest CFG. It exchanged the
  outer and inner induction-variable initial values and bounds in place, which
  gives each source index the other index's domain when ranges differ. Although
  it was registered at `-O2`, `-Os`, `-O3`, and `-Ofast`, the audited source
  probe did not activate it; retaining the false transform as an optimization
  was unjustified. It is no longer exported or registered. Pipeline-roster
  tests pin that decision, and an asymmetric `3 x 5` source regression verifies
  the source index domains at `-O0`, `-O2`, and `-O3` before any future real CFG
  interchange may be admitted.

## Tranche 13: Full closeout and release recertification

Status: `[~]` — local Apple-Silicon recertification is complete; a fresh
pushed multi-runner CI cycle and its exact run IDs remain outstanding.

- `[x]` Re-run targeted O0/O3 reproducers for every fixed semantic finding.
- `[x]` Run `cargo test --workspace` and `cargo clippy --workspace`.
- `[~]` Run the supported OpenMP matrix and standalone afs-as/afs-ld suites on
  all available CI runners. The complete Apple-Silicon workspace, official
  OpenMP O0/O3 examples/V&V, and exact native toolchain gates are green; fresh
  pushed Linux/x86 evidence is pending.
- `[x]` Run the current stdlib high-rank/shared/ILP64/O3 configuration through
  the intended linker path.
- `[x]` Re-run the selected fpm/f-toml and other large-project compiler-edge
  builds used before this audit.
- `[x]` Reconcile all Low, policy, coverage, and plausible items in this
  ledger; no severity tier is implicitly waived.
- `[~]` Record release readiness and any externally blocked platform-only
  evidence with exact CI run IDs.

Local closure evidence at root commit
`4601667da480337bb7f216cac81702347a278d16`:

- Every semantic fix remains paired with its focused O0/O3 or compile-fail
  regression. The full `test_programs` matrix passed at O0, O1, O2, O3, Os,
  and Ofast, including the loop-cleanup, processor-kind, `C_SIZEOF`, parser,
  OpenMP nesting/reduction/collapse, I/O, `.amod`, and optimizer-audit cases.
- `cargo test --workspace` passed in full: the principal summaries include
  1,193 afs-as unit tests, 2,248 compiler library tests, 1,338 CLI/driver
  tests, 73 multi-file tests, 161 `test_programs` harness tests, 405 runtime
  tests, and all workspace doc tests. The six full program sweeps exercised
  more than 34,000 compile/link/run subprocess identities.
- `cargo clippy --workspace -- -D warnings -A clippy::too_many_arguments`
  passed. The sole allowance is the repository's existing deliberate API
  shape exception.
- The pinned official OpenMP example build/run set and OpenMP V&V set passed
  at O0 and O3. The standalone afs-as and afs-ld Apple-Silicon exact gates
  account for all 40 and 57 native skip identities respectively, and their
  complete available suites passed again within the workspace run.
- stdlib commit `b1670f03a69872ad2a8a2eed2003470c2fa27651`
  built rank 15, shared, ILP64, and O3. All 29 dylib links were traced through
  `afs-ld`. The disposable source carries one upstream-only F2018 C8105 fix
  (`PRIVATE :: process_callback` before its forward interface); flang agrees
  the unmodified ordering is nonconforming. Reconfiguration also proved that
  unsupported `REAL128` is now rejected instead of being advertised as a
  usable kind.
- fpm v0.13.0 commit `90bb83a70e9bcf04d941fb43cca014ae1c0fc5ea`
  built from the official amalgamation, passed 35/35 CLI, 61/61 help, and
  12/12 new-project tests, then self-hosted through two further O3/afs-ld
  stages. Stage 2 and stage 3 were byte-identical with SHA-256
  `9b317389d7ea86d4e377383e4f1094fa30954a27f328c5b59037f31698431d0e`.
- fgof-toml commit `8cdf25f0` built its unmodified sources through fpm at O3;
  all six test programs linked through traced afs-ld invocations and passed.
  Its direct CMake route remains a project toolchain-identification mismatch:
  CMake classifies ARMFORTAS as GNU and injects unsupported warning flags.
- fortsh commit `f8310e7b4c8a1dfc9f22b7be127e725674a02d54`
  built all 64 release sources at O2 into an 11 MiB arm64 Mach-O executable,
  with the final link traced through afs-ld. Its smoke target passed, followed
  by 23/23 quick POSIX suites: 3,632 passed, 0 failed, and 7 documented skips.
- fortsh exposed and drove one additional `.amod` edge during certification:
  an exported `TYPE(C_FUNPTR), PARAMETER` initialized from `C_NULL_FUNPTR`
  wrote a scalar-zero payload that the reader rejected. Commit `4601667d`
  now round-trips only the valid null C-pointer representation and rejects
  arbitrary non-null pointer bits, with codec and separate-compilation runtime
  regressions.

Release disposition: no Audit 13 finding remains open on Apple Silicon. A
release-candidate decision still requires pushing this branch, obtaining green
fresh CI across the configured runners, and recording those exact run IDs
here.

## Closed audit dispositions that must remain documented

- `[x]` Opening `SINGLE NOWAIT` was independently refuted as a defect; OpenMP
  5.2 permits it and both tested compilers accept it.
- `[x]` Existing substring `CHECK`, `STDERR_CHECK`, and `ERROR_EXPECTED`
  behavior matches its documented contract; the missing exact alternative is
  tracked as Coverage G01 rather than changing existing directive semantics.
- `[x]` Empty-command `EXECUTE_COMMAND_LINE` was scope-adjusted to a processor
  policy inconsistency and remains tracked as Low L01.
- `[x]` `.amod` `CONTIGUOUS` loss was confirmed, but no separate downstream
  wrong result was claimed beyond the demonstrated `ASYNCHRONOUS` gate bypass.
