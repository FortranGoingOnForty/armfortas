# Adversarial audit index

Date: 2026-07-09

Implementation baseline: `23857aa48f3bc0160303842488e8578acb487fb1`

Pinned toolchain revisions:

- `afs-as`: `fac26fb9c1c4064b9bf838e393fc1d7363ff3409`
- `afs-ld`: `615de762090c8a9c73033ca1659b021cefe4331d`
- `bencch`: `8da8e1da967b5a641da822e8aff180587be93a33`

## Verdict

Twelve independent review fronts reported 174 confirmed discrepancies. After
collapsing 11 cross-report root-cause groups, the current tree has **162 unique
findings: 15 Critical, 94 High/Major, 49 Medium/Moderate, and 4 Low/Minor**.

One of those 162, the dead-store elimination scaling defect in A04-09, is an
exact reconfirmation of a previously tracked open issue. The campaign therefore
adds **161 newly distinct findings** while supplying a stronger reduction for
the known DSE problem.

The result is not a statement that normal compilation is universally broken.
The six-level program corpus remained green, and the focused package suites
passed. It is evidence that the passing corpus leaves major semantic, ABI,
runtime, object-format, and CI contracts uncovered. Several failures produce
silent wrong code or successful but unusable binaries.

## Report index

Severity labels are normalized here: `Major` is High, `Moderate` is Medium,
and `Minor` is Low. A12-13 is counted once at its higher, Medium severity.

| Report | Front | Critical | High | Medium | Low | Raw |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| [01](audit-01.md) | Driver, preprocessing, lexer, parser | 1 | 9 | 6 | 0 | 16 |
| [02](audit-02.md) | Semantic analysis, modules, submodules | 0 | 10 | 7 | 0 | 17 |
| [03](audit-03.md) | Parsed-program to IR lowering | 2 | 4 | 2 | 0 | 8 |
| [04](audit-04.md) | Optimizer legality and scaling | 1 | 6 | 2 | 0 | 9 |
| [05](audit-05.md) | ARM64 code generation | 0 | 7 | 1 | 0 | 8 |
| [06](audit-06.md) | x86_64 code generation and SysV ABI | 0 | 4 | 2 | 0 | 6 |
| [07](audit-07.md) | Allocation, cleanup, finalization | 0 | 14 | 4 | 0 | 18 |
| [08](audit-08.md) | Runtime I/O, IEEE, system behavior | 0 | 15 | 6 | 0 | 21 |
| [09](audit-09.md) | `afs-as` ARM64, x86, Mach-O, ELF | 0 | 5 | 13 | 1 | 19 |
| [10](audit-10.md) | `afs-ld` Mach-O and ELF | 10 | 16 | 1 | 1 | 28 |
| [11](audit-11.md) | Test and CI integrity | 1 | 3 | 5 | 0 | 9 |
| [12](audit-12.md) | Reproducibility and performance | 0 | 6 | 7 | 2 | 15 |
| **Raw total** | | **15** | **99** | **56** | **4** | **174** |
| **Deduplicated** | | **15** | **94** | **49** | **4** | **162** |

## Critical findings

| ID | Failure |
| --- | --- |
| AUD01-001 | Multi-file compilation resets target and preprocessing options, so a cross-target command can emit host objects or compile different source branches. |
| A03-01 / RM-14 | Implicit cleanup destroys an allocatable derived value before invoking its finalizer. |
| A03-02 / RM-16 | Rank-specific FINAL procedures receive element storage instead of the required array descriptor and are called the wrong number of times. |
| A04-03 | Loop fusion assumes distinct pointer descriptors do not alias and emits observably wrong code. |
| A10-A1 | `afs-ld` destroys mixed positional/library/framework order and can bind a call to a different provider. |
| A10-M1 | Mach-O rebase streams omit pointers in custom segments, leaving loader-visible addresses unfixed. |
| A10-M3 | Imported Mach-O `UNSIGNED` relocation addends are discarded. |
| A10-M4 | Mach-O ICF conflates same-numbered section referents from different objects. |
| A10-M5 | Dead stripping fails to root initializer sections and section-level retention flags. |
| A10-M6 | Incompatible same-name Mach-O sections merge and initialized bytes can disappear. |
| A10-M7 | Same-address aliases can detach the configured entry symbol from code under dead stripping. |
| A10-E3 | ELF shared libraries are always treated as `--as-needed`, dropping constructor-only and registration dependencies. |
| A10-E7 | Dynamic ELF initializer, preinitializer, and finalizer arrays have no dynamic tags. |
| A10-E8 | Executable-defined GNU IFUNCs are called as ordinary functions in dynamic links. |
| A11-01 | FreeBSD's only end-to-end compiler sweep pipes Cargo through `tee` without `pipefail`, masking real test failures. |

## High-risk clusters

The Critical list is not the complete urgent queue. The following High/Major
clusters also produce broad wrong-code, ABI, or data-integrity failures:

- **Procedure boundaries:** ARM64 tail calls expose destroyed overflow
  arguments, entry copies overwrite an incoming i128 pair, stale NZCV drives
  wide selects, and large-frame i128 stores destroy a limb. x86 entry copies
  overwrite later GP/XMM arguments, narrow returns and actuals are not
  canonicalized, and `OPTIONAL, VALUE` has no presence state.
- **Optimizer legality:** global value facts survive side-effecting calls;
  fission and fusion use invalid independence assumptions; bounds-check
  elimination drops narrowing semantics; constant folding and CSE cross
  floating-environment changes; fusion output is process-randomized.
- **Language semantics:** USE/IMPORT controls, assignment type checking,
  explicit-interface argument checking, submodule ancestry, and separate
  module procedure contracts are incomplete or ignored.
- **Resource lifecycle:** STAT-less failures continue, multi-object status is
  overwritten, MOVE_ALLOC and allocatable assignment skip finalization,
  component ownership is shallow-copied, and allocation-size overflow either
  aborts despite STAT or falsely succeeds.
- **Runtime I/O:** ordinary INQUIRE can overwrite adjacent storage; logical
  input is omitted; read errors are erased; null fields shift assignments;
  formatted real input ignores descriptor state; writes discard errors;
  NAMELIST EOF hangs while holding global I/O state.
- **Assembler and linker:** ARM64 `sp` arithmetic encodes as `xzr`; immediate
  fields narrow or spill into opcodes; x86 `testq` and `ret` silently change
  meaning; weak relaxation resolves incorrectly; many Mach-O/ELF symbol,
  relocation, archive, dead-strip, and dynamic-loader contracts remain open.
- **Reproducibility and scaling:** module result bounds, fusion choice, local
  COMMON order, and Mach-O UUID identity are unstable or under-keyed. Liveness,
  ARM allocation rewriting, archive extraction, symbol caching, block lookup,
  and DSE contain quadratic or worse structures.

## Deduplication map

These are one root-cause family each, despite appearing in more than one
front-specific report:

| Canonical family | Reported as |
| --- | --- |
| Multi-source object identity | AUD01-002, A02-17, A12-05 |
| Closing program-unit name validation | AUD01-015, A02-15 |
| Finalization after destruction | A03-01, RM-14 |
| Rank-specific FINAL dispatch | A03-02, RM-16 |
| Cleanup of owned allocatable components | A03-05, RM-15 |
| One-site inlining and the 32-round fixpoint cap | A04-08, A12-02 |
| Custom Cargo target artifact discovery | AUD01-010, A11-05 |
| Content-insensitive Mach-O UUID | A10-M10, A12-07 |
| Random local-COMMON ELF symbol order | A09-16, A12-08 |
| Linear Mach-O relocation symbol lookup | A09-17, A12-09 |
| Release compiler selecting the debug runtime | A11-02, A12-12 |

Related findings that require separate fixes were not collapsed. Examples
include the assembler and linker independently discarding executable-stack
intent, distinct optimizer nondeterminism sites, and separate imported versus
executable-defined IFUNC failures.

## Historical reconciliation

- A04-09 is the already tracked DSE alias-query blowup, now independently
  reduced and source-traced. It is not counted among the 161 newly distinct
  findings.
- A02-09 is related to historical C26 because both widen `USE, ONLY` behavior
  across `.amod` boundaries, but they use different paths: dependency-edge
  serialization versus defined-assignment candidate lookup.
- A04-04 is a new nondeterministic loop-fusion site. It is not the previously
  recorded C37/C42 unswitch/codegen site; repeated builds split into two
  runtime behaviors.
- A05-08 records a repository contract conflict. The optimization API says
  value-changing floating-point transforms are Ofast-only, while
  `x09-pass-audit.md` explicitly permits ARM FMA contraction at O2. The project
  must select one public policy before treating either side as authoritative.
- The prior x86 audit addendum says FINAL gaps were closed. Current baseline
  reproducers still show destroyed-state finalization, rank-blind dispatch,
  missing parent/dynamic finalization, and duplicate final calls. That closure
  statement is stale.
- A08-15 extends the historical byte-transparency family to command arguments
  and environment values; it is not the same call path as the prior non-UTF-8
  OPEN filename finding.
- A10-E8 concerns IFUNCs defined by the executable. Historical L5 concerns
  imported shared-library IFUNC classification; both remain distinct ABI work.

## Validation performed

- The six-level end-to-end corpus ran 747 programs per level: 745 passed and
  two expected failures remained XFAIL at each level.
- Root functional integration tests passed after building the runtime in the
  hard-coded default target directory. Two timing guardrails failed only under
  concurrent audit load and each passed three serial reruns.
- `afs-as` and `afs-ld` package test suites passed on this Linux host. Platform
  tests that clean-skip remain coverage gaps documented in report 11.
- The exact CI Clippy command passed. Adding `--all-targets` failed on current
  test code. `cargo fmt --all -- --check` failed with drift in 55 files.
- Five lowering failures were independently rerun from preserved cases. The
  fusion nondeterminism probe produced both correct and wrong binaries across
  20 fresh processes.
- The `.amod` result-bound probe produced two interface forms across 16 fresh
  processes (11 without bounds, five with bounds).
- Representative ELF linker checks confirmed dropped non-needed DSOs, missing
  initializer tags, and missing local IFUNC resolution. Corrected GNU controls
  retained the DSO, emitted initializer tags, and resolved the IFUNC to 42.
- Allocation multiplication overflow was rerun with both runtime profiles: the
  debug runtime aborted across an extern-C boundary, while release returned
  successful allocated state with a null payload.

## Limits

- The host was x86_64 Linux. ARM64/macOS output was checked through IR,
  assembly, LLVM decoding, and object metadata, not native execution.
- FreeBSD-specific behavior was source-traced against its workflow and ABI;
  no FreeBSD runtime was available locally.
- Most Mach-O linker findings are complete source-path traces with
  self-contained fixtures but still need native Apple loader confirmation.
- Miri, Valgrind, and `cargo fuzz` were unavailable. The checked-in fuzz targets
  are not run by CI.
- Performance findings use structural complexity and output-shape evidence;
  wall-clock measurements taken while parallel auditors were active were not
  used.

The original checkout and its repo-root `verify-*` and
`verify-audit-scratch/` directories were not modified. Audit probes remain in
their report-named `/tmp/armfortas-audit*` directories for remediation work.
