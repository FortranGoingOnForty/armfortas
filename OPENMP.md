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
| Directive syntax model | Partial | `parallel`, `do`, `parallel do`, and `critical` plus the initial clause set have typed AST forms. |
| Unsupported syntax handling | Supported | Malformed directives are errors; recognized but unimplemented execution is rejected before IR lowering. |
| `parallel` | Preview | Owned synchronous fork/join runtime, implicit join, nested serialized regions, `if`, and `num_threads`. |
| Data sharing | Partial | `shared`, `private`, `firstprivate`, `default(shared)`, and `default(none)` for the scalar and array forms accepted by semantic validation. |
| Scalar data | Partial | INTEGER, REAL, DOUBLE PRECISION, and LOGICAL. Character, derived, pointer, allocatable, optional, volatile, and asynchronous scalar cases remain restricted. |
| Array data | Partial | Numeric/logical constant explicit-shape storage and selected descriptor-backed dummy, allocatable, pointer, and section views. Unsupported ownership or lifetime cases are diagnosed. |
| Worksharing loops | Not implemented | `do` and `parallel do` are parsed but cannot execute yet. Scheduling, chunking, `collapse`, and `nowait` are not supported. |
| Reductions and synchronization | Not implemented | No reductions, barriers, critical execution, atomics, flush, locks, ordered regions, single, masked, or sections yet. |
| Runtime library | Partial | Initial `omp_lib` queries/setter and timing routines only. Unsupported API names are not published as implemented procedures. |
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

## Next milestones

1. Finish pointer-array `private` and `firstprivate` association and lifetime
   semantics.
2. Implement canonical worksharing `do` and combined `parallel do`, beginning
   with static scheduling and implicit barriers.
3. Add reductions and named/unnamed `critical`, then dynamic scheduling.
4. Validate the resulting host subset with FERP and NPB EP before widening the
   public support claim.
5. Build the durable memory-model primitives needed for barriers, atomics,
   flush, locks, and the wider worksharing surface.

[omp-52]: https://www.openmp.org/spec-html/5.2/openmp.html
[omp-60]: https://www.openmp.org/wp-content/uploads/OpenMP-API-Specification-6-0.pdf
[omp-examples]: https://www.openmp.org/specifications/
[omp-vv]: https://github.com/OpenMP-Validation-and-Verification/OpenMP_VV
