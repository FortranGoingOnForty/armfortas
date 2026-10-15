# Audit 13: semantic correctness, OpenMP conformance, and release confidence

Date: 2026-10-15

Reviewed root commit: `2f758760d514f05f8cf858d1a4f952fbc2ce825a`

Pinned submodules:

- `afs-as` at `4514f677f6da13f576820449e4b3d3f3973ee6b5`
- `afs-ld` at `8a8a8b91825541675df76558473f67a0b1ec2765`
- `bencch` at `00c2d172e12e895bd6d3d9fac622566b7c83ccdb`

Host: Apple Silicon, macOS 26.4.1 (25E253)

Release compiler SHA-256:
`ee863c02f3e2543b654b75fff0433da71e7bee0e7c4edf6236ec15db7dc1dff0`

CI baseline: run `38671923760`, green at the reviewed root commit.

## Executive result

ARMFORTAS has a broad, working compiler pipeline and an unusually large
regression corpus, but this audit found seven independently reproduced High
defects in valid-language cleanup/intrinsic semantics, parser fail-closed
behavior, and OpenMP nesting validation. The most important valid-program
wrong-code issue is skipped finalization when `EXIT` or `CYCLE` crosses a
`BLOCK`. The most dangerous accepted-invalid-program issues are two OpenMP
nesting gaps that produce deterministic deadlocks.

The next tier contains separate-compilation metadata loss, incomplete
intrinsic-module type/kind modeling, runtime I/O semantic gaps, an OpenMP
initial-team reduction failure, and several test/release paths that can report
green for the wrong reason. These are real release-confidence constraints even
though the pinned CI run is green.

The standalone assembler/linker review also confirmed object-format
invariant and compatibility issues. They are important to the project's
end-to-end toolchain goal, but they do not all affect the default native Mach-O
driver path: this checkout still uses Apple `as` and `ld` unless explicitly
overridden. They are tracked separately below so their priority is based on
confirmed reachability rather than static severity alone.

## Method and evidence boundary

The audit used six broad discovery lanes followed by independent verification
of the surviving semantics/runtime, OpenMP, test/release, and toolchain
candidates. Reviewers inspected the committed baseline read-only. Dynamic
reproducers were confined to the audit scratch directory and were run at `-O0`
and `-O3` where optimization could alter the result. GNU Fortran and the host C
compiler were used only as reality checks; the Fortran and OpenMP specifications
remain the semantic authority.

Retained evidence for this run is under:

```text
/private/tmp/armfortas-audit-20261015.Uhl6Ft/
```

The manifest records hashes, lane state, exact commands, platform information,
and the prior-audit reconciliation. Individual reports are under `evidence/`;
dynamic logs and source reproducers are under `logs/` and `repro/`.

### Coverage map

| Lane | Principal surface | Result |
|---|---|---|
| Frontend/sema | preprocess, lexer, parser, AST, validation, intrinsic modules, `.amod` | eight promoted findings; prior frontend closures sampled |
| IR/optimizer/codegen | lowering, cleanup ownership, intrinsics, optimizer legality, ARM64 ABI/codegen | three High findings; two optimizer hypotheses retained as unverified |
| Driver/runtime | CLI lifecycle, I/O, process/runtime discovery, temporary ownership, diagnostics | three Medium and several Low findings |
| OpenMP | parsing, close nesting, lowering, runtime teams/reductions, documented subset | two High, three Medium, one Low; one proposed finding refuted |
| afs-as/afs-ld | Mach-O/ELF object contracts, producer/consumer parity, publication behavior | four Medium and one Low confirmed; one publication policy decision |
| Test/release | root harness, differentials, skip gates, benchmarks, packaging, release smoke | four Medium findings plus Low defects and explicit coverage gaps |
| Independent verification | separate semantics/runtime, OpenMP, toolchain, and test/release reviews | all ranked findings reproduced or fully traced; impacts adjusted conservatively |

## Confirmed High findings

### A13-01 — Loop control crossing `BLOCK` skips cleanup and finalization

Valid `EXIT` and `CYCLE` statements branch directly to loop targets without
leaving intervening lexical cleanup scopes. Finalizable local objects therefore
record zero finalizations where GNU Fortran records one for `EXIT` and three
for `CYCLE`; the result is unchanged at `-O3`.

- Control sites: `src/ir/lower/ctx.rs:148-154,526-539` and
  `src/ir/lower/stmt.rs:7404-7424`.
- Cleanup construction: `src/ir/lower/stmt.rs:9265-9308`.
- Impact: silent wrong results, skipped user finalizers, and skipped implicit
  allocatable cleanup on ordinary valid Fortran control flow.
- Fix: retain the lexical-cleanup depth in each loop scope and route every
  loop exit/cycle edge through the cleanup-edge helper.

This is the strongest first remediation tranche because it is valid-program
wrong behavior, has a narrow lowering root cause, and admits focused O0/O3
regressions for named and unnamed loops.

### A13-02 — Runtime and optional `SELECTED_*_KIND` calls return fabricated kinds

The runtime lowering does not implement the same selection rules as constant
resolution. A nonconstant `SELECTED_INT_KIND` falls back to kind 4;
`SELECTED_REAL_KIND` considers only its first argument and ignores `R`,
`RADIX`, absent arguments, and their defined negative result codes. Equal
literal and runtime values therefore disagree.

- Control site: `src/ir/lower/intrinsic.rs:1654-1710`.
- Related constant logic: `src/sema/types.rs:691-713`.
- Reproduced outputs included ARMFORTAS `integer 8 4` where the two results
  must agree, and `4 -1 8 4` where the reference results were `8 8 8 -5`.
- Fix: one authoritative processor-kind capability table and selection
  algorithm, shared by constant evaluation and runtime lowering, with complete
  optional/keyword slot canonicalization.

### A13-03 — Aggregate `C_SIZEOF` returns pointer width

`C_SIZEOF` is computed from already-lowered IR values. Arrays and derived
objects have decayed to pointers at that point, so both a three-element
`INTEGER(C_INT)` array and a `BIND(C)` object with three C integers report 8
bytes instead of the host ABI's 12 bytes at O0 and O3.

- Control site: `src/ir/lower/intrinsic.rs:1605-1627`.
- Impact: under-allocation, truncated copies, and incorrect foreign-interface
  lengths.
- Fix: compute from semantic type, extent, and interoperable layout before
  address decay; cover padded records and runtime extents.

### A13-04 — Invalid explicit `CALL` expressions are silently erased

The parser accepts an unrestricted expression after `CALL`; semantic analysis
returns early for a non-name callee; lowering has no rejecting fallback.
`CALL 1 + 2` therefore compiles, emits no call, and execution continues.

- Parser: `src/parser/stmt.rs:915-941`.
- Validation: `src/sema/validate/core.rs:1228-1240`.
- Lowering: `src/ir/lower/stmt.rs:5407-5991`.
- Fix: parse only procedure-designator forms, reject other shapes in sema, and
  assert the invariant in lowering.

### A13-05 — Derived-type parsing silently discards unrecognized source

Unknown component-part and type-bound-part lines are consumed through the end
of the statement without recording a diagnostic. A type containing arbitrary
text consequently compiles and is used as though the line did not exist.

- Component path: `src/parser/decl.rs:1224-1236`.
- Type-bound path: `src/parser/decl.rs:1030-1067`.
- Fix: diagnose before synchronization and add no-output-publication tests for
  invalid component lines, binding lines, and attributes.

A13-04 and A13-05 share a fail-open frontend root cause and should be fixed as
one small parser-hardening tranche immediately after A13-01.

### A13-06 — `BARRIER` inside `SINGLE` or `ORDERED` is accepted

OpenMP validation omits `SINGLE` and `ORDERED` from the prohibited enclosing
regions for an explicit barrier. The `SINGLE` reproducer compiles and times out
with four threads at O0 and O3; GNU Fortran rejects it. The ordered path has the
same structural deadlock: one ordered entrant waits at a team barrier while
other iterations cannot enter the ordered region.

- Validation: `src/sema/validate/openmp.rs:277-296`.
- Lowering: `src/ir/lower/openmp.rs:624-630,790-811`.
- Runtime barrier: `runtime/src/openmp.rs:736-755`.
- Fix: centralize OpenMP region categories and close-nesting relations instead
  of maintaining construct-specific string deny-lists.

### A13-07 — Same-name nested `CRITICAL` is accepted and self-deadlocks

No ancestry/name check rejects a critical region nested anywhere within a
critical region of the same case-insensitive name. Unnamed critical regions
share the same implicit name. The runtime lock is deliberately non-reentrant,
so the accepted one-thread program times out at both O0 and O3.

- Validation: `src/sema/validate/openmp.rs:131-134`.
- Lowering: `src/ir/lower/openmp.rs:879-900`.
- Runtime lock: `runtime/src/openmp.rs:1097-1167`.
- Fix: store structured OpenMP ancestry frames, including normalized critical
  names, and reject any enclosing match rather than checking only proximity.

## Confirmed Medium findings

### A13-08 — `.amod` drops `ASYNCHRONOUS` and `CONTIGUOUS`

Version 18 module-variable records do not carry either attribute. A combined
source correctly rejects a shared asynchronous variable because the OpenMP
memory-model support is incomplete, while the same provider and consumer built
separately are accepted. `CONTIGUOUS` loss is also certain, although this audit
did not prove an additional downstream wrong result for it.

- Writer/schema: `src/sema/amod.rs:815-892,1727-1753,2570-2815`.
- Import: `src/sema/resolve/use_resolution.rs:810-878`.
- Defeated gate: `src/sema/validate/openmp.rs:2846-2854`.
- Fix: add both fields, reconstruct them on import, bump the exact format
  version, and compare combined and split compilation in tests.

### A13-09 — `ISO_FORTRAN_ENV` kind metadata is not truthful

Two related surfaces derive from hard-coded declarations rather than the
compiler's actual capability model:

- `CHARACTER_KINDS`, `INTEGER_KINDS`, `LOGICAL_KINDS`, and `REAL_KINDS` are
  scalar constants instead of rank-one arrays. `SIZE` calls compile into
  unresolved external `_size` calls.
- `REAL128` is advertised as positive kind 16 even though `REAL(16)` is
  explicitly rejected and `SELECTED_REAL_KIND(33)` returns `-1`.

Control sites are `src/sema/intrinsic_modules.rs:67-95,275-290` and the
kind-limited representation in `src/ir/types.rs:61-77,227-232`. Populate all
kind constants and arrays from the same capability model used to fix A13-02.

### A13-10 — IEEE nominal types are erased to INTEGER

Declarations of `IEEE_CLASS_TYPE`, `IEEE_ROUND_TYPE`, and related standard
types collapse to integer semantic types. Cross-assignment between class and
round values therefore compiles although the nominal types are distinct.

- Registration/resolution: `src/sema/intrinsic_modules.rs:331-359,405-419`
  and `src/sema/resolve/type_resolution.rs:16-32,222-231`.
- Fix: preserve distinct semantic identities and map them to integer storage
  only below type checking.

### A13-11 — Invalid `OPEN` enum values silently select defaults

Unknown values for `STATUS`, `ACTION`, `ACCESS`, `FORM`, and `POSITION` are
mapped to ordinary defaults and return `IOSTAT=0`. All five cases were
reproduced; the reference runtime returned a nonzero status and a diagnostic.

- Runtime: `runtime/src/io_system.rs:1104-1204,1247-1258`.
- Fix: validate every limited-value specifier before opening or mutating unit
  state; honor `IOSTAT`/`IOMSG` without side effects on failure.

### A13-12 — List-directed input omits repeat counts and successful slash termination

Legal `3*7` input returns an error and leaves values unchanged. Slash correctly
stops assignment but still returns an error rather than successful
termination. The common tokenizer has no repeat expansion or terminal-slash
state.

- Runtime family: `runtime/src/io_system.rs` list-directed tokenization and
  scalar readers; detailed call graph is retained in the verifier report.
- Fix: implement repeat/null token state once in the common tokenizer and
  propagate terminal slash as success to every typed reader.

### A13-13 — An explicit static runtime override can be ignored

A nonexistent `AFS_RUNTIME_PATH` still yields a linked and runnable executable
because the driver falls back to another runtime archive. Explicit overrides
must either select the requested artifact or fail.

- Driver/runtime discovery: see the exact trace in
  `evidence/verify_semantics_runtime/report.md` V12.
- Fix: distinguish explicit override from automatic discovery and make the
  explicit branch fail closed.

### A13-14 — Worksharing `DO`/`LOOP` close nesting is not enforced

A `DO` inside `SINGLE` is accepted. With `NOWAIT`, only the single executor's
scheduled slice runs, producing 2 rather than 8 iterations; without it the
path can hang at the inner barrier. `LOOP` follows the same validation and
lowering route.

- Validation: `src/sema/validate/openmp.rs:73-113,1177-1191`.
- Lowering: `src/ir/lower/openmp.rs:572-598`.
- Fix with A13-06 using one close-nesting relation model.

### A13-15 — Standalone initial-team reductions abort

Standalone worksharing dispatch treats execution as a one-thread initial team,
but reduction completion requires an explicit runtime team and returns
`AFS_OMP_ERROR_NO_TEAM`. A valid scalar addition reduction error-stops at O0
and O3; GNU Fortran produces 20.

- Lowering: `src/ir/lower/openmp.rs:1809-1822,2062-2079,2642-2647`.
- Runtime: `runtime/src/openmp.rs:1552-1574`, with the same pattern in the
  other supported reduction families.
- Fix: represent the implicit initial team consistently or provide a complete
  one-thread reduction path.

### A13-16 — Nonrectangular `COLLAPSE(2)` reconstruction is cubic

For a triangular nest with `Theta(M^2)` logical iterations, every flattened
iteration rescans preceding rows to reconstruct its indices. Aggregate mapping
work is `Theta(M^3)` before executing the user body.

- Lowering: `src/ir/lower/core.rs:32087-32126`.
- Runtime: `runtime/src/openmp.rs:2493-2507,2550-2572`.
- Fix: invert the monotone prefix function in constant or logarithmic time and
  test operation counts rather than wall-clock timing.

### A13-17 — Active XFAIL accepts any failure stage

Four active XFAIL annotations turn every compile, link, run, output, and
reproducibility error into the same passing result. An unrelated new regression
can therefore hide behind the known debt.

- Harness: `tests/run_programs.rs:2451-2888`.
- Fix: represent failure stage/kind structurally and bind every XFAIL ID to an
  expected stage plus a stable signature.

### A13-18 — The x86 assembler differential permits a large compile-skip budget

Every failed compiler `-S` invocation is counted as a skip, including ordinary
non-diagnostic programs. Its 1,550 comparison floor leaves 745 of 2,295
nominal non-diagnostic source/optimization cells unaccounted at this baseline.

- Harness: `tests/x86_afs_as_differential.rs:242-374`.
- Fix: define the exact expected identity set and permit compiler failures only
  for explicitly classified diagnostic/debt fixtures.

### A13-19 — Standalone-tool skip gates do not prove exact coverage

The afs-as gate validates only broad output shape. The afs-ld gate validates
record syntax and duplicate IDs but has no reviewed platform-specific identity
and count manifest. Tests may vanish or turn into new skips without failing the
gate.

- Gates: `ci/check_afs_as_skips.sh` and pinned `afs-ld/ci/check_skips.sh`.
- Fix: compare exact `(suite,test,count)` multisets for each platform.

### A13-20 — Direct release packaging can mix a Git label with worktree bytes

The source packager enumerates tracked paths through Git but copies their live
filesystem contents. Direct callers can therefore create an archive named for
one commit that contains modified root or submodule bytes. The current official
Actions job starts from a fresh checkout, so this is not evidence that current
official artifacts are contaminated.

- Script: `scripts/package-release-source.sh:38-103`.
- Fix: export committed blobs, or reject every tracked root/submodule
  worktree/index difference before packaging.

### A13-21 — ELF COMDAT group selection is absent

Valid `SHT_GROUP`/`SHF_GROUP`/`GRP_COMDAT` relationships are not represented.
The group section and membership map are dropped while allocation members are
retained and merged independently. Duplicate grouped code/data and grouped
constructor contributions can therefore remain, or a strong signature can
produce a duplicate-definition failure rather than selecting one group.

- Parser/output: `afs-ld/src/elf.rs:17-59,375-453,2453-2525,3763-3828`.
- Fix: resolve each COMDAT signature before ordinary section/symbol
  resolution, discard losing groups atomically, and carry that decision
  through archives, GC, static output, and dynamic output.

This is the strongest standalone-toolchain remediation because it affects
ordinary valid compiler-generated ELF, not only extreme file sizes or invalid
input.

### A13-22 — afs-as extended section numbering is rejected by afs-ld

afs-as deliberately emits the standard `e_shnum=0`/section-zero encoding when
the section count reaches the reserved range and has a committed round-trip
test. afs-ld reads the raw 16-bit value and rejects zero before recovering the
true value from section zero.

- Producer: `afs-as/src/elf.rs:720-768,1016-1030,1517-1548`.
- Consumer: `afs-ld/src/elf.rs:323-353,641-659`.
- Fix: decode section zero first, recover both extended fields, then apply
  checked table/range validation. Add a direct producer-to-consumer test.

### A13-23 — High-index ELF symbols are serialized with reserved/wrapped indices

afs-as casts a real symbol section index to `u16` without emitting a
`SHT_SYMTAB_SHNDX` side table. Indices in the reserved range become special
values and higher indices can wrap to undefined or another section.

- Writer: `afs-as/src/elf.rs:674-683,806-845`.
- Reader gap: `afs-as/src/elf.rs:1187-1225`.
- Fix with A13-22: implement the symbol-index side table and test symbols and
  relocations below, at, and above the reserved boundary.

### A13-24 — ARM64 relocation prefixes are incompletely validated

The linker fuses `ARM64_RELOC_ADDEND` and `SUBTRACTOR` prefixes after dropping
or ignoring part of their address/width/flag contract. The sibling assembler
enforces those constraints, but the linker even has a positive test for an
invalid `ADDEND + UNSIGNED` pair.

- Parser/validation: `afs-ld/src/reloc/mod.rs:217-312,389-462,733-744`.
- Producer contract: `afs-as/src/macho.rs:764-845,1525-1585`.
- Scope: independently downgraded to invalid-input validation; no conforming
  producer path was established.
- Fix: retain and validate complete prefix metadata before fusion, then replace
  the contradictory success test with focused rejection tests.

## Confirmed Low findings

- A zero-length `EXECUTE_COMMAND_LINE` command returns `CMDSTAT=1` before the
  selected command processor sees it. The Fortran interpretation is processor
  dependent, so this is an implementation inconsistency rather than a broad
  conformance failure.
- Successful `--emit-tokens`, `--emit-ast`, and `--emit-ir` terminal modes omit
  `--time-report` output.
- Failure to launch an explicitly selected linker exits with the documented
  I/O category 3 rather than linker category 2.
- A failed multi-input build leaves `TMPDIR/afs_multi_<pid>/source_0.o`; the
  suspected concurrent collision was not independently demonstrated.
- Six-digit statement labels are accepted although a Fortran statement label
  has at most five digits.
- `_OPENMP` and `omp_lib::openmp_version` correctly use the standardized 5.2
  date value `202111`, but publishing that value overstates an explicitly
  partial preview implementation.
- The benchmark gate can bootstrap a missing baseline and tolerates missing
  rows; current committed baselines are complete.
- The release smoke accepts any output containing `42`, not the complete
  expected stream.
- The official OpenMP source guard checks metadata strings rather than source
  content hashes.
- Benchmark cleanup does not preserve whitespace in its temporary-directory
  argument.
- README claims native builds use afs-as by default, while the current native
  Mach-O driver uses Apple `as`; the claim is accurate only for the ELF route.
- The public Mach-O load-command reader accepts noncanonical but valid dylib
  and RPATH string offsets, while its writer canonicalizes offsets/padding and
  violates the documented byte-round-trip invariant. Current final-image
  emission synthesizes new commands, so no final-link semantic effect was
  established.

## Standalone toolchain frontier

This lane is worth pursuing because end-to-end afs-as/afs-ld ownership is part
of the project's definition of done. Independent review confirmed three
valid-input Medium defects, one invalid-input Medium validation defect, and one
Low wire-round-trip contract violation. The correct bounded order is:

1. Implement or loudly reject ELF group/COMDAT semantics before describing the
   ET_REL path as general compiler-object compatibility.
2. Reconcile ELF extended section numbering and high-index symbol ownership
   across afs-as and afs-ld as one design.
3. Validate ARM64 paired-relocation prefixes before fusion. This is important
   ABI-boundary hardening, but its confirmed trigger is a nonconforming object.
4. Preserve or explicitly constrain legal noncanonical Mach-O load-command
   string layouts so the reader/writer's documented byte-round-trip invariant
   remains true.
5. Decide whether primary output and requested link-map publication are one
   transaction. A bounded CLI probe confirmed that a map-publication failure
   can occur after a valid primary image has been installed.

These paths are opt-in on the reviewed native configuration. They should not
displace A13-01 through A13-07, but A13-21 through A13-23 should precede
lower-severity runtime and release-harness cleanup.

## Plausible or unverified observations

- Optimizer alias answers are point-based where dead-store elimination needs
  overlapping byte-range reasoning. No source-level wrong-code reproducer was
  established.
- Loop interchange appears to swap loop metadata rather than transform the
  nest. Reachability and a semantic difference were not established.
- `.amod` parsing accepts some self-consistent records that could be rejected
  more strictly. No producer-generated or cross-unit semantic failure was
  demonstrated.

## Refuted and adjusted claims

- Opening `SINGLE NOWAIT` is valid OpenMP 5.2 syntax. ARMFORTAS and GNU Fortran
  both accept and execute it. It is not a defect.
- The empty-command `EXECUTE_COMMAND_LINE` issue was downgraded because command
  interpretation is processor dependent.
- `CONTIGUOUS` loss in `.amod` is confirmed metadata loss, but this audit did
  not assign it an independent wrong-code consequence beyond the demonstrated
  `ASYNCHRONOUS` validation bypass.
- Substring `CHECK`, `STDERR_CHECK`, and `ERROR_EXPECTED` directives behave as
  documented. The missing exact-stream alternative is a coverage limitation,
  not a violation of their existing contract.

## Root-cause groups

1. Semantic truth is duplicated after representation has already been erased.
   This drives the kind-selection split, aggregate `C_SIZEOF`, hard-coded
   intrinsic-module metadata, and IEEE nominal-type loss.
2. Several parser/runtime recovery paths substitute omission or a default for
   an invalid explicit construct. Invalid calls, derived-type lines, and
   `OPEN` values therefore fail open instead of producing hard diagnostics.
3. Control scopes retain targets but not all ownership/region metadata needed
   at their edges. Loop cleanup and OpenMP nesting checks are the clearest
   consequences.
4. Cross-boundary schemas are narrower than the semantic model. `.amod`
   attributes, initial-team reduction state, and ELF producer/consumer index
   handling each lose information at a subsystem boundary.
5. Several CI gates compare thresholds or generic failure shape rather than
   exact identities. That permits XFAIL, compile-skip, and platform-skip false
   greens despite a healthy-looking aggregate count.
6. The standalone tools implement adjacent pieces of object formats without a
   shared conformance matrix. COMDAT, extended numbering, high-index symbols,
   and relocation-prefix validation need explicit cross-tool contract tests.

## Prior-audit reconciliation

Audit 32's 33-item closeout ledger is marked complete. Eleven focused
release-mode regressions spanning its CLI, ABI, parser, semantic, and driver
closures passed at this baseline without skips. The historical `-g` suffix
item is only partially closed: broad `-g*` parsing now normally emits a generic
warning, but `-w` can suppress it. This is a known residual, not a novel Audit
13 finding.

No prior audit document was overwritten. In particular,
`.docs/audits/audit33.md` remains the historical April sprint-readiness audit;
this document continues the later `audit-00` through `audit-12` series.

## Coverage gaps

- This host directly proves only Apple Silicon behavior. Cross-platform claims
  rely on the pinned CI evidence or portable static traces.
- CI builds the bencch Rust crates but does not execute the 55 authored `.afs`
  suites (358 cases). Three nested `test_programs/future` fixtures are also
  outside the root harness's non-recursive discovery. This is a visible policy
  gap, not proof of a hidden implementation defect.
- The audit sampled historical closures rather than rerunning every historical
  reproducer.
- Broad standard-library and application builds remain essential because the
  audit cannot enumerate the full Fortran standard surface.

## Recommended remediation sequence

1. Fix A13-01 and land named/unnamed `EXIT`/`CYCLE` cleanup regressions at O0
   and O3.
2. Fix the fail-open parser pair A13-04/A13-05 with hard diagnostics and
   no-output-publication tests.
3. Unify kind selection/capability semantics for A13-02/A13-09, then fix
   aggregate `C_SIZEOF` (A13-03).
4. Centralize OpenMP nesting validation and close A13-06/A13-07/A13-14 in one
   bounded semantic tranche.
5. Repair initial-team reductions (A13-15), then replace cubic collapsed-loop
   reconstruction (A13-16).
6. Bump `.amod` for A13-08 and preserve IEEE nominal identities for A13-10.
7. Close runtime behavior A13-11/A13-12/A13-13.
8. Remediate the standalone-toolchain frontier in its own small commits:
   COMDAT first, extended ELF numbering/symbol indices second, then relocation
   prefix validation.
9. Tighten XFAIL, differential, skip, and release gates before claiming a
   release candidate from green CI alone.

The next implementation branch should start with A13-01. It is the highest
confidence valid-program semantic failure and has the smallest defensible
patch surface among the High findings.
