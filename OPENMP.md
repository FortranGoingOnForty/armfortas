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
| Directive syntax model | Partial | `parallel`, `do`, `parallel do`, and `critical` plus their implemented clauses have typed AST forms. |
| Unsupported syntax handling | Supported | Malformed directives are errors; recognized but unimplemented execution is rejected before IR lowering. |
| `parallel` | Preview | Owned synchronous fork/join runtime, implicit join, nested serialized regions, `if`, and `num_threads`. |
| Data sharing | Partial | `shared`, `private`, `firstprivate`, `lastprivate`, `default(shared)`, `default(private)`, and `default(none)` for the scalar and array forms accepted by semantic validation. Predetermined named constants and assumed-size arrays remain shared under `default(private)`; explicit clauses retain precedence. `lastprivate` is currently limited to supported intrinsic scalars on worksharing loops. |
| Scalar data | Partial | INTEGER, REAL, DOUBLE PRECISION, LOGICAL, fixed-length default-kind CHARACTER, selected allocatable/pointer forms, and nonpolymorphic derived-type privatization. Unsupported ownership, dynamic-length, optional, volatile, and asynchronous cases are diagnosed. |
| Array data | Partial | Numeric, logical, and fixed-length default-kind CHARACTER constant-shape storage plus selected descriptor-backed dummy, allocatable, pointer, section, and shared derived-type views. Unsupported ownership or lifetime cases are diagnosed. |
| Worksharing loops | Preview | Canonical `do` and combined `parallel do`, positive/negative strides, static contiguous and chunked schedules, combined dynamic schedules, rectangular and direct-bound nonrectangular `collapse(2)`, implicit barriers, standalone `nowait`, scalar `lastprivate`, and predefined scalar/fixed-rank-one reductions. General affine nonrectangular bounds and nonrectangular `lastprivate` remain diagnosed; standalone dynamic scheduling remains rejected. |
| Reductions and synchronization | Partial | Scalar objects and constant-explicit-shape rank-one arrays of INTEGER support `+`, `*`, `max`, `min`, `iand`, `ior`, and `ieor`; REAL/DOUBLE PRECISION support `+`, `*`, `max`, and `min`; COMPLEX supports `+` and `*`; and LOGICAL supports `.and.`, `.or.`, `.eqv.`, and `.neqv.` on `parallel`, combined `parallel do`, and standalone worksharing `do`. Whole arrays, constant contiguous unit-stride sections, and constant elements are accepted. Partial selections reject whole-array and provably out-of-selection references within the region; dynamic element references retain bounds enforcement. Narrow integers combine at their storage width, and REAL(4)/REAL(8) and COMPLEX(4)/COMPLEX(8) combine at their declared precision. Standalone `do reduction` supports result publication at its implicit barrier and barrier-free `nowait` completion. Atomics, flush, locks, ordered regions, `single`, `masked`, the `sections` construct, higher-rank/runtime-shaped arrays, and user-defined reductions remain unimplemented. |
| Runtime library | Partial | Initial `omp_lib` queries, setters, timing routines, team execution, barriers, work dispatch, reductions, and critical locks. Unsupported API names are not published as implemented procedures. |
| Environment variables | Partial | Initial handling for `OMP_NUM_THREADS`, `OMP_DYNAMIC`, `OMP_THREAD_LIMIT`, `OMP_MAX_ACTIVE_LEVELS`, and `OMP_SCHEDULE`. |
| SIMD semantics | Not implemented | `-fopenmp-simd` does not yet change loop vectorization or accept a SIMD construct as executable support. |
| Tasking | Not implemented | Tasks, dependences, task groups, and task loops are future work. |
| Device offload | Not implemented | No `target`, device mapping, teams offload, or plugin runtime. Host execution is not presented as offload fallback support. |
| Tool interfaces | Not implemented | OMPT and OMPD are not implemented. |

The compiler also rejects otherwise supported constructs when their bodies use
operations whose outlined-procedure ABI or thread safety has not been secured.
Those diagnostics are part of the preview contract.

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

The pinned official-example corpus uses twelve unchanged Fortran sources
from OpenMP Examples v6.0.1 at revision
`3e4757ae2b52e51df6cd5d363d6fc1d894509719`. Its manifest covers `parallel`,
`do`, `schedule(static)`, `nowait`, `private`, `firstprivate`, `lastprivate`,
`shared`, `default(private)`, rectangular and direct-bound nonrectangular
`collapse(2)`, predefined reductions, runtime thread queries, assumed-size
sequence forwarding, dynamically bound orphaned worksharing, fixed-form shared
DO termination labels, and named and unnamed critical regions.
Every source completes its upstream-requested compile or link operation at
`-O0` and `-O3`; the upstream run example executes at both levels. The corpus,
license, revision record, and harness live under `tests/fixtures/openmp-official/` and
`tests/openmp_official_examples.rs`.

An orphaned worksharing loop obtains its current thread and team size from the
runtime when its procedure is called, while lexically nested worksharing keeps
using the outlined callback parameters. Wider affine nonrectangular bounds
remain a separate diagnosed extension beyond the direct-bound form exercised
by `collapse.4`.

## Next milestones

1. Add applicable host cases from the OpenMP Validation and Verification suite
   under the same pinned-source and manifest discipline.
2. Run NPB EP Class S through the owned runtime and preserve minimized compiler
   edges as focused regressions.
3. Extend array reductions beyond constant-shape rank one only with a truthful
   descriptor and partial-storage mapping design.
4. Add guided/runtime scheduling only after each worksharing construct owns an
   explicit dispatch-generation protocol.
5. Build the durable memory-model primitives needed for barriers, atomics,
   flush, locks, and the wider worksharing surface.

[omp-52]: https://www.openmp.org/spec-html/5.2/openmp.html
[omp-60]: https://www.openmp.org/wp-content/uploads/OpenMP-API-Specification-6-0.pdf
[omp-examples]: https://www.openmp.org/specifications/
[omp-vv]: https://github.com/OpenMP-Validation-and-Verification/OpenMP_VV
