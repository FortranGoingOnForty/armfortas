# OpenMP support

ARMFORTAS has an experimental OpenMP host-execution preview. It is not yet a
conforming implementation of the complete OpenMP API, and release material
must not describe it as generic or complete OpenMP support.

The implementation baseline is the [OpenMP 5.2 specification][omp-52]. With
OpenMP processing enabled, ARMFORTAS defines `_OPENMP=202111`. That value
selects the syntax and semantic baseline implemented by the compiler; during
the preview it does not supersede the feature matrix below or constitute a
claim of full 5.2 conformance. The AST and runtime ABI are designed to grow
toward [OpenMP 6.0][omp-60] without silently changing existing behavior.

## Enabling the preview

- `-fopenmp` enables conditional compilation, directive processing, host
  parallel execution, and the OpenMP runtime.
- `-fno-openmp` disables OpenMP processing.
- `-fopenmp-simd` enables OpenMP conditional compilation and SIMD-directive
  processing, but SIMD execution semantics are not implemented yet.
- Without either enabling flag, OpenMP directive and conditional-compilation
  sentinels retain their ordinary comment behavior.

## Feature matrix

| Area | Status | Current boundary |
|---|---|---|
| Free- and fixed-form sentinels | Supported | Includes source-form-correct directive continuation and conditional compilation. |
| Directive syntax model | Partial | `parallel`, `do`, `parallel do`, `loop`, `single`, `sections`, block and stand-alone doacross `ordered`, `barrier`, `flush`, `master`, `critical`, `atomic read`, `atomic write`, `atomic update`, and the declarative `threadprivate` directive plus their implemented clauses have typed AST forms. |
| Unsupported syntax handling | Supported | Malformed directives are errors; recognized but unimplemented execution is rejected before IR lowering. |
| `parallel` | Preview | Owned synchronous fork/join runtime, implicit join, nested serialized regions, `if`, and `num_threads`. |
| Data sharing | Partial | `shared`, `private`, `firstprivate`, `lastprivate`, bounded `copyin`, `default(shared)`, `default(private)`, and `default(none)` for the scalar and array forms accepted by semantic validation. `copyin` copies the primary thread's supported THREADPRIVATE intrinsic scalar and constant-explicit-shape array values into the other team members before `parallel` or combined `parallel do` execution. Predetermined named constants and assumed-size arrays remain shared under `default(private)`; explicit clauses retain precedence. OpenMP 5.0 `loop` associated iterators are predetermined `lastprivate`, with `private` and `lastprivate` accepted as explicit overrides. Other `lastprivate` support is currently limited to supported intrinsic scalars on worksharing loops. |
| Scalar data | Partial | INTEGER, REAL, DOUBLE PRECISION, COMPLEX, LOGICAL, fixed-length default-kind CHARACTER, selected allocatable/pointer forms, and nonpolymorphic derived-type privatization. Ordinary scalar dummies may be `private` or `firstprivate`; unsupported ownership, dynamic-length, optional, volatile, and asynchronous cases are diagnosed. |
| Array data | Partial | Numeric, complex, logical, and fixed-length default-kind CHARACTER constant-shape storage plus selected descriptor-backed dummy, allocatable, pointer, section, and shared derived-type views. A nonoptional explicit-shape dummy with runtime bounds may be `private`; its encounter-time shape is snapshotted and each implicit task receives independent storage. `firstprivate` and assumed-shape dummy arrays remain outside that boundary. Unsupported ownership or lifetime cases are diagnosed. |
| `threadprivate` | Partial | Module INTEGER, REAL, DOUBLE PRECISION, COMPLEX, and LOGICAL scalars and constant-explicit-shape arrays use runtime-owned per-thread storage. Rank-one deferred-shape REAL and DOUBLE PRECISION module pointers without non-NULL initialization are also supported. Worker copies and pointer associations persist across qualifying equal-team top-level parallel regions when dynamic teams are disabled, and the descriptor-backed representation survives separate compilation through `.amod`. `copyin` is supported for the nonpointer intrinsic scalar and constant-explicit-shape forms, including snapshots above the compiler's 64 KiB stack threshold; pointer `copyin` is diagnosed. Allocatable, other pointer, derived, character, and COMMON-block THREADPRIVATE forms remain diagnosed or unsupported. |
| Worksharing loops | Preview | Canonical `do` and combined `parallel do`, positive/negative strides, static contiguous and chunked schedules, combined dynamic schedules, rectangular and direct-bound nonrectangular `collapse(2)`, implicit barriers, standalone `nowait`, scalar `private` and `lastprivate`, predefined scalar/fixed-rank-one reductions, and one directly nested unconditional block `ordered` region per logical iteration under the default or static schedule. Two-dimensional `ordered(2)` doacross loops support perfect rectangular nests, constant nonzero increments, the default or static schedule, affine predecessor vectors, and both the OpenMP 5.2 `doacross(source/sink:...)` spelling and deprecated `depend(source/sink:...)` synonym. Other parameter depths, doacross with collapse, nonrectangular doacross nests, conditional or multiple block-ordered regions, and ordered SIMD remain diagnosed. OpenMP 5.0 `loop` supports scalar `private`, associated-iterator `lastprivate`, scalar INTEGER `reduction(+:...)`, and rectangular `collapse(1)`/`collapse(2)` when it binds implicitly to an enclosing `parallel` region. Predetermined iterator copy-out follows the sequential termination value; `private(iterator)` suppresses it, and an explicit `lastprivate` list item must be associated with the construct. Other `loop` clauses and reduction forms, explicit `bind`, nonrectangular `loop collapse`, general affine nonrectangular bounds, and nonrectangular `lastprivate` remain diagnosed; standalone dynamic scheduling remains rejected. |
| Reductions and synchronization | Partial | Scalar objects and constant-explicit-shape rank-one arrays of INTEGER support `+`, `*`, `max`, `min`, `iand`, `ior`, and `ieor`; REAL/DOUBLE PRECISION support `+`, `*`, `max`, and `min`; COMPLEX supports `+` and `*`; and LOGICAL supports `.and.`, `.or.`, `.eqv.`, and `.neqv.` on `parallel`, combined `parallel do`, and standalone worksharing `do`. OpenMP 5.0 `loop` currently supports only scalar INTEGER addition reductions. Whole arrays, constant contiguous unit-stride sections, and constant elements are accepted on the broader constructs. Partial selections reject whole-array and provably out-of-selection references within the region; dynamic element references retain bounds enforcement. Narrow integers combine at their storage width, and REAL(4)/REAL(8) and COMPLEX(4)/COMPLEX(8) combine at their declared precision. Standalone `do reduction` supports result publication at its implicit barrier and barrier-free `nowait` completion. Block `ordered` regions serialize in logical iteration order across repeated worksharing generations. Two-dimensional doacross source events publish completed logical coordinates and sink events wait for every in-domain predecessor; out-of-domain sink vectors are ignored. `single` selects the primary thread as its implementation-defined executor, has the required default exit barrier, and supports `nowait`; `private`, `firstprivate`, `copyprivate`, and `allocate` remain outside the bounded slice. Standalone and orphaned `sections` assign every nonempty structured block to exactly one implicit task with deterministic static ownership, have the required default exit barrier, support `nowait`, and bind serial calls to the initial one-thread team. `private`, `firstprivate`, `lastprivate`, `reduction`, `allocate`, and combined `parallel sections` remain outside this first bounded slice. Explicit `barrier` synchronizes the current team, including from an orphaned procedure; the initial one-thread team completes immediately, and prohibited close nesting is diagnosed. Explicit and orphaned `master` regions bind to the current team, execute only on thread zero, and have no implicit barrier; serial calls bind to the initial one-thread team. `atomic read` and `atomic write` support scalar non-CHARACTER intrinsic variables and side-effect-free array elements; `atomic update` supports `x = x + expr` for scalar INTEGER, REAL, DOUBLE PRECISION, and COMPLEX storage. All three use a conservative process-wide lock. Bare `flush`, `flush(list)`, and explicit `flush seq_cst` use a whole-memory sequentially consistent fence; the standard permits the list form to be strengthened this way. Simple blocking locks support `omp_init_lock`, `omp_destroy_lock`, `omp_set_lock`, and `omp_unset_lock` with `integer(omp_lock_kind)` storage and acquire/release synchronization. Atomic capture/compare, weaker memory-order clauses, `omp_test_lock`, hinted initialization, nested locks, wider doacross dimensions/forms, `masked`, higher-rank/runtime-shaped reductions, and user-defined reductions remain unimplemented. |
| Runtime library | Partial | Initial `omp_lib` queries, setters, timing routines, simple blocking locks, team execution, barriers, sequentially consistent flush fences, work dispatch, block-ordered and two-dimensional doacross generations, reductions, critical/atomic locks, and module `threadprivate` storage. Implemented no-argument inquiry and timing routines and reference-passed simple lock routines also export their standard symbol spellings for legacy `EXTERNAL` declarations; other argument-bearing routines remain on the typed module lowering path. Unsupported API names are not published as implemented procedures. |
| Environment variables | Partial | Initial handling for `OMP_NUM_THREADS`, `OMP_DYNAMIC`, `OMP_THREAD_LIMIT`, `OMP_MAX_ACTIVE_LEVELS`, and `OMP_SCHEDULE`. |
| SIMD semantics | Not implemented | `-fopenmp-simd` does not yet change loop vectorization or accept a SIMD construct as executable support. |
| Tasking | Not implemented | Tasks, dependences, task groups, and task loops are future work. |
| Device offload | Not implemented | No `target`, device mapping, teams offload, or plugin runtime. Host execution is not presented as offload fallback support. |
| Tool interfaces | Not implemented | OMPT and OMPD are not implemented. |

The compiler also rejects otherwise supported constructs when their bodies use
operations whose outlined-procedure ABI or thread safety has not been secured.
Those diagnostics are part of the preview contract.

Branches whose targets are wholly inside the same OpenMP structured block are
preserved when the region is outlined. A `GOTO`, computed `GOTO`, or arithmetic
`IF` with any target outside that block remains a hard error; ordinary Fortran
structured-control checks independently reject branches into nested or sibling
constructs.

## Conformance policy

OpenMP conformance is broader than accepting directives. It includes directive
and clause semantics, runtime routines, internal control variables, environment
variables, the memory model, tool interfaces, and thread-safe behavior of the
underlying Fortran implementation and its intrinsic/library procedures.
ARMFORTAS will claim only the individual features in the matrix until that
complete contract is met.

Every newly supported slice requires frontend diagnostics, IR and optimizer
coverage, runtime tests, native ARM64 macOS and x86_64 ELF execution, and
differential checks against established implementations where behavior is
deterministic. Applicable cases from the [official examples][omp-examples] and
the [OpenMP Validation and Verification suite][omp-vv] are added incrementally
rather than treated as an opaque pass/fail corpus.

The pinned official-example corpus uses fifteen unchanged Fortran sources
from OpenMP Examples v6.0.1 at revision
`3e4757ae2b52e51df6cd5d363d6fc1d894509719`. Its manifest covers `parallel`,
`do`, `schedule(static)`, `nowait`, `private`, `firstprivate`, `lastprivate`,
`shared`, `default(private)`, `threadprivate`, `copyin`, rectangular and
direct-bound nonrectangular `collapse(2)`, predefined reductions, runtime
thread queries, assumed-size sequence forwarding, dynamically bound orphaned
worksharing, `sections` with two structured blocks and its implicit barrier,
fixed-form shared DO termination labels, and named and unnamed critical
regions. Every source completes its upstream-requested compile, link,
or run operation at `-O0` and `-O3`; run cases also assert their deterministic
numeric output. The corpus, license, revision record, and harness live under
`tests/fixtures/openmp-official/` and `tests/openmp_official_examples.rs`.

An orphaned worksharing loop obtains its current thread and team size from the
runtime when its procedure is called, while lexically nested worksharing keeps
using the outlined callback parameters. Wider affine nonrectangular bounds
remain a separate diagnosed extension beyond the direct-bound form exercised
by `collapse.4`.

An explicit `barrier` likewise binds dynamically to the current team, so it can
reside in a procedure whose source is lexically outside a `parallel` region.
When such a procedure is called serially, the initial one-thread team makes the
barrier a no-op. A barrier may not be closely nested inside a worksharing
`do`, `loop`, combined `parallel do`, `sections`, `critical`, `master`, or
`atomic update` region.

The pinned OpenMP V&V corpus starts with three unchanged OpenMP 5.0 `loop`
host cases: `test_loop_private.F90`, `test_loop_reduction_add.F90`, and
`test_loop_collapse.F90`, all from revision
`f7d95b342b9330ecac735ed31b9fb193d8f60a9b`. Each source is SHA-256 pinned and
runs at `-O0` and `-O3` using a documented host-only compatibility header; the
upstream umbrella header also contains unsupported device-offload probes.
These cases cover implicit binding to an enclosing `parallel` region, per-loop
scalar privatization, scalar INTEGER addition reduction, rectangular collapse
depths one and two, preservation of sequential semantics outside the collapse
depth, preprocessing constants inside directive lines, and completion
synchronization. Their license, manifest, adapter, and harness live under
`tests/fixtures/openmp-vv/` and `tests/openmp_vv.rs`.

NASA NPB 3.4.4 EP Class S also builds from unchanged benchmark sources and
verifies successfully through the owned runtime. A clean `-O0` build passes
with four threads, and a clean `-O3` build passes with one, two, four, and eight
threads. Every run reports the reference histogram
`6140517, 5865300, 1100361, 68546, 1648, 17, 0, 0, 0, 0`; the floating-point
reduction sums differ only in their expected final digits as the team size
changes. The sweep preserved focused regressions for implicit-external runtime
queries, module `threadprivate`, numeric atomic updates, OpenMP `master`,
external-procedure capture classification, and the mixed fixed/assumed
CHARACTER calling convention exercised by NPB's report routine.

NASA NPB 3.4.4 CG Class S now also builds from unchanged benchmark sources and
verifies successfully through the owned runtime. Independent clean `-O0` and
`-O3` builds each pass with one, two, four, and eight threads, reporting the
reference zeta `8.5971775078648`. This exercises static worksharing, scalar
reductions, eight explicit barriers (six in orphaned procedures), and both
lexically nested and orphaned `master` regions. Focused regressions preserve
the two final frontend fixes: I/O `*` syntax is not a data-environment capture,
and an orphaned `master` binds dynamically to the current team.

NASA NPB 3.4.4 FT Class S builds from unchanged benchmark sources and verifies
successfully through the owned runtime. Independent clean `-O0` and `-O3`
builds each pass with one, two, four, and eight threads, and every run matches
all six reference complex checksums. The sweep exercises shared COMPLEX(4) and
COMPLEX(8) arrays, scalar complex reduction, `collapse(2)`, and private complex
explicit-shape dummy arrays whose bounds include module and dummy values.
Focused regressions preserve contextual use of the identifier `class`, scalar
dummy privatization, encounter-time shape capture, independent private storage,
and cleanup that never deallocates the caller's descriptor.

NASA NPB 3.4.4 MG Class S now also builds from unchanged benchmark sources and
verifies successfully through the owned runtime. Independent clean `-O0` and
`-O3` builds pass at one, two, four, and eight threads. The O3 run matrix
reports the reference L2 norm `0.5307707005735E-04`; only the expected final
roundoff digits of the reported error vary with team size. MG exercises a
repeated `do ordered schedule(static)` region, including both positive and
negative worksharing elsewhere in the application. Focused parser, semantic,
runtime, and native O0/O3 regressions preserve logical-iteration sequencing
across repeated ordered generations.

NASA NPB 3.4.4 BT and SP Class S also build from unchanged benchmark sources
and verify successfully through the owned runtime. Independent clean `-O0`
and `-O3` builds of each application pass at one, two, four, and eight threads.
Neither probe required a compiler change: together they broaden the validated
surface to two sustained array-heavy alternating-direction implicit solvers
using static worksharing, rectangular collapse, reductions, `master`, and
module `threadprivate` storage.

NASA NPB 3.4.4 LU Class S now builds from unchanged benchmark sources and
verifies successfully through the owned runtime. Independent clean `-O0` and
`-O3` builds pass at one, two, four, and eight threads; the reference surface
integral is `0.7841892886594E+01`. LU introduced three bounded synchronization
frontiers. Its three bare `single` regions now execute once with the required
implicit barrier. Scalar intrinsic `atomic read` and `atomic write` use the
same process-wide identity as atomic updates. Bare and list-form `flush`
directives lower to the standard-permitted stronger whole-memory `seq_cst`
fence. Focused parser, semantic, runtime, diagnostic, and native O0/O3 tests
preserve those boundaries.

The alternate NASA NPB 3.4.4 LU `VERSION=DOAC` source also builds unchanged and
verifies at `-O0` and `-O3` with one, two, four, and eight threads. Its
two-dimensional `ordered(2)` loops use both ascending and descending nests and
the deprecated `depend(source/sink:...)` spelling. The runtime keeps these
coordinate dependencies separate from block-ordered sequencing. Atomic
capture/compare, weaker memory orders, SINGLE data clauses, wider doacross
dimensions, nonrectangular nests, and nonconstant doacross steps remain
diagnosed.

NASA NPB 3.4.4 UA Class S with `VERSION=AU` now builds from unchanged benchmark
sources and verifies at `-O0` and `-O3` with one, two, four, and eight threads.
Every run matches the reference temperature integral
`0.1890013110962E-02`. This irregular adaptive-mesh workload introduced compact
`PARALLELDO` end spellings, left-associated chained atomic additions, and the
simple lock API used by its shared mesh updates. Focused parser, semantic,
runtime, diagnostic, and native O0/O3 tests preserve those boundaries. The
lock representation is the public eight-byte `omp_lock_kind` object itself;
initialization and destruction do not allocate hidden process state.

The unchanged default NASA NPB 3.4.4 UA Class S path also builds and verifies
at `-O0` and `-O3` with one, two, four, and eight threads. It replaces AU's
atomic-heavy transfer implementation with extensive per-mortar simple-lock
updates and therefore validates distinct lock-object identity under sustained
contention. Every run matches the same reference temperature integral. On the
validation Apple Silicon host, the O3 benchmark time falls from 4.29 seconds
with one thread to 1.36 seconds with eight threads; this timing is evidence for
the bounded lock implementation, not a portable performance guarantee.

The `VERSION=RD` NASA NPB 3.4.4 UA Class S path now builds unchanged and
verifies at `-O0` and `-O3` with one, two, four, and eight threads. RD's manual
reduction associates two rank-one REAL(8) `threadprivate` pointer descriptors
with per-thread columns of shared work arrays, retains the other
`threadprivate` state across consecutive parallel regions, and combines those
columns through an orphaned `do nowait` routine. Every run matches the reference
temperature integral `0.1890013110962E-02`. This probe also found a general
`.amod` ABI bug: an imported module pointer array was reconstructed as an
eight-byte pointer slot instead of the 392-byte descriptor used in its defining
unit. A cross-unit O0/O3 regression now preserves the descriptor representation.

On the validation Apple Silicon host, RD's O0 times at one, two, four, and eight
threads were 19.42, 10.01, 5.63, and 3.58 seconds; its O3 times were 4.20, 2.29,
1.46, and 1.37 seconds. These measurements are validation evidence, not portable
performance guarantees.

### Bounded THREADPRIVATE `copyin`

`copyin` on `parallel` and combined `parallel do` now supports the existing
nonpointer module THREADPRIVATE boundary: INTEGER, REAL, DOUBLE PRECISION,
COMPLEX, and LOGICAL scalars and constant-explicit-shape arrays. The compiler
freezes each primary-thread value in the synchronous outlined environment
before launching the team. Every non-primary callback then assigns that
snapshot into its runtime-resolved logical-worker slot before executing the
structured block; the primary callback deliberately retains its live copy.

Separate-compilation regressions run at `-O0` and `-O3` and cover scalar
INTEGER, LOGICAL, and COMPLEX values, a small REAL(8) array, and a 72 KiB
REAL(8) array that crosses the compiler's private-array stack threshold. They
also verify persistence in a following equal-team region and the combined
`parallel do` spelling. Non-THREADPRIVATE list items and pointer `copyin` are
hard errors. Pointer association copying remains outside this boundary until
its ownership contract is designed explicitly.

The unchanged official `copyin.1.f` compile example covers scalar INTEGER and
REAL `copyin` beside a pointer THREADPRIVATE allocation in a called external
procedure. The unchanged `threadprivate.6.f` run example allocates a rank-one
REAL pointer independently in every implicit task, calls across an implicit
external interface, and verifies a sum of `100.0` from every requested thread
at `-O0` and `-O3`. The pinned OpenMP V&V revision contains no Fortran source
using `copyin` or `threadprivate`, so no V&V case is claimed for this slice.

### Bounded `sections`

Standalone and orphaned `sections` constructs support one or more nonempty
structured blocks separated by `section` directives. An initial `section`
marker is optional. Each block has a deterministic static owner computed from
its zero-based index modulo the current team size, so it executes exactly once
without a shared dispatch cursor or reusable generation state. The construct
has an implicit exit barrier unless `nowait` is present; serial calls use the
initial one-thread team.

Focused `-O0` and `-O3` tests exercise seven sections on four threads, repeated
constructs, `nowait` followed by an explicit barrier, orphaned procedures, and
serial execution. The unchanged official `fort_loopvar.2.f90` compile example
covers two sections followed by `single` in one parallel region. The pinned
OpenMP V&V revision contains no Fortran `sections` source, so no V&V case is
claimed. Data-environment clauses, `lastprivate`, reductions, allocation
clauses, and combined `parallel sections` remain diagnosed.

## Next milestones

1. Extend `sections` with bounded `private` and `firstprivate` data environments,
   then keep `lastprivate`, `reduction`, allocation, and combined
   `parallel sections` as independently testable slices.
2. Extend array reductions beyond constant-shape rank one only with a truthful
   descriptor and partial-storage mapping design.
3. Add guided/runtime scheduling only after each worksharing construct owns an
   explicit dispatch-generation protocol.
4. Extend atomic capture/compare and acquire/release flush semantics only with
   explicit memory-order tests; the current conservative lock and `seq_cst`
   fence are truthful but intentionally stronger than those future forms.

[omp-52]: https://www.openmp.org/spec-html/5.2/openmp.html
[omp-60]: https://www.openmp.org/wp-content/uploads/OpenMP-API-Specification-6-0.pdf
[omp-examples]: https://www.openmp.org/specifications/
[omp-vv]: https://github.com/OpenMP-Validation-and-Verification/OpenMP_VV
