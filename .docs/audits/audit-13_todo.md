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

Status: `[ ]`

- `[ ]` A13-17: represent harness failures by stage/kind and bind each XFAIL
  ID to an expected failure signature.
- `[ ]` A13-18: require successful `-S` for every ordinary x86 differential
  cell and compare an exact expected identity set.
- `[ ]` A13-19: give afs-as and afs-ld exact per-platform skip manifests.
- `[ ]` Coverage G01: add opt-in exact stdout/stderr/diagnostic directives and
  migrate tests whose complete stream is the contract.
- `[ ]` Coverage G02: choose and enforce a CI tier for the 55 bencch suites and
  inventory the nested future fixtures.

## Tranche 11: Release and benchmark fail-closed behavior

Status: `[ ]`

- `[ ]` A13-20: package committed root/submodule blobs or reject tracked dirty
  state before constructing a release archive.
- `[ ]` Low L07: make benchmark comparison require an existing exact baseline;
  reserve baseline creation for an explicit update mode.
- `[ ]` Low L08: make the release smoke compare normalized complete output and
  require empty stderr.
- `[ ]` Low L09: content-hash every pinned official OpenMP source.
- `[ ]` Low L10: quote and guard benchmark temporary cleanup correctly.
- `[ ]` Low L11: correct native-vs-ELF afs-as routing documentation, or change
  routing and prove the new default in a subprocess test.

## Tranche 12: Convert plausible observations into decisions

Status: `[ ]`

- `[ ]` U01: establish or refute source reachability for overlapping-byte
  alias answers in dead-store elimination; fix and test if reachable.
- `[ ]` U02: establish or refute loop-interchange reachability and semantic
  transformation; fix or remove/disable the pass if its contract is false.
- `[ ]` U03 is tracked with `.amod` strictness in Tranche 6.
- `[ ]` Record every refutation with a complete source trace and durable test
  where regression risk remains.

## Tranche 13: Full closeout and release recertification

Status: `[ ]`

- `[ ]` Re-run targeted O0/O3 reproducers for every fixed semantic finding.
- `[ ]` Run `cargo test --workspace` and `cargo clippy --workspace`.
- `[ ]` Run the supported OpenMP matrix and standalone afs-as/afs-ld suites on
  all available CI runners.
- `[ ]` Run the current stdlib high-rank/shared/ILP64/O3 configuration through
  the intended linker path.
- `[ ]` Re-run the selected fpm/f-toml and other large-project compiler-edge
  builds used before this audit.
- `[ ]` Reconcile all Low, policy, coverage, and plausible items in this
  ledger; no severity tier is implicitly waived.
- `[ ]` Record release readiness and any externally blocked platform-only
  evidence with exact CI run IDs.

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
