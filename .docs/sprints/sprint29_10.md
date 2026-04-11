# Sprint 29.10: Sprint 29 Cleanup & Completion

## Why This Sprint Exists

Sprint 29 sprawled. The implementation now contains a real optimizer surface,
but the planning docs overstate what is finished, understate what is still only
metadata or scaffolding, and the test surface is still much stronger on runtime
output than on IR/object/binary verification.

Sprint 29.10 is the cleanup sprint that closes Sprint 29 for real. Its job is:
- identify every unfinished item across 29, 29.5, 29.6, 29.7, 29.8, and 29.9
- finish them one by one in priority order
- harden the optimizer with a brutal audit while we do it

The audit is not a separate side quest. It is part of how we finish the cleanup sprint
without carrying false confidence into Sprint 30+.

## Sprint 29 Status

### 29.5: Performance & Cleanup

Mostly landed:
- `Function::value_type()` is O(1) via the type cache
- `AcValue::ImpliedDo` is boxed
- preprocessor emission tracking uses `skip_depth`

Still missing or partial:
- `integer(16)` / `I128` is still only partially staged: raw IR, globals, local `-O0`
  codegen, ABI-visible params/returns/calls, and full optimizer-level support through
  `-Ofast` now exist for the current scalar surface, but broader wide-value surface
  area is still missing
- preprocessor expansion is still split across `expand_condition_macros` and
  `expand_macros_inner`

### 29.6: Loop Optimizations

Partially landed:
- loop fusion, fission, interchange, peeling, unswitching, and unrolling are
  present and wired into the live optimization pipeline
- runtime corpus contains real loop-focused programs

Still missing:
- there is no actual NEON/SIMD vectorization pass module
- whole-array vectorization remains lowering + scalar loop code, not vector IR/codegen

### 29.7: Function Inlining

Mostly landed for same-module IR:
- inlining exists with O1/O2/O3 thresholds
- call graph exists and drives bottom-up inlining
- recursive inlining is blocked conservatively

Still missing:
- cross-module inlining remains blocked behind multi-file / whole-program work
- the aggressive O3 story is mostly "larger threshold" rather than full hot-path IPO

### 29.8: Advanced IR Optimizations

Strongest part of the sprint and largely real:
- `GVN`
- `SROA`
- alias analysis
- cross-block load-store forwarding
- bounds-check elimination

Recent hardening completed:
- optimized capture stages now respect requested opt levels
- O3/Ofast regained `GVN`
- BCE is end-to-end from lowering through runtime and elimination
- mem2reg chained-promotion verifier bug fixed
- contained dummy-array lowering bug fixed
- cross-block LSF hardened against call/path/loop-header hazards

Remaining 29.8 work is now audit and hardening work, not missing greenfield implementation.

### 29.9: Fortran-Specific & Interprocedural Optimizations

Mostly not landed as optimizer work:
- PURE/ELEMENTAL flags are preserved on IR functions, but no optimizer pass uses them
- `DO CONCURRENT` lowers as an ordinary sequential loop
- no whole-array vectorization pass exists
- no dead-argument elimination pass exists
- no constant-argument propagation pass exists
- no return-value propagation pass exists
- no whole-program analysis pass exists

Partial runway exists:
- statically-known type-bound calls already lower directly to a concrete callee
- call graph and inlining infrastructure exist
- base alias analysis exists and can be extended in a Fortran-aware direction

## What Sprint 29.10 Must Finish

Priority order for cleanup:

### 1. Finish the real missing work from 29.9

This is the biggest unfinished frontier and should be the main implementation focus:
- Fortran-aware no-alias exploitation beyond today's narrow AA consumer set
- PURE/ELEMENTAL exploitation
- `DO CONCURRENT` optimization licensing
- whole-array vectorization path
- IPO pieces that are actually possible before Sprint 30

Practical starting point:
- PURE call CSE / reuse
- Fortran-AA consumers
- `DO CONCURRENT`-specific optimization behavior

Whole-program IPO and cross-module work should be finished only where the current
single-file surface makes that honest; otherwise they remain blocked behind Sprint 30.

### 2. Finish the non-29.9 leftovers that still block honest Sprint 29 closure

- `integer(16)` / `I128` still needs broader wide-value support beyond the newly landed
  scalar + stack-passed direct-call ABI surface
- preprocessor codepath unification from 29.5 is still unfinished
- 29.6 still lacks a general native NEON/SIMD loop vectorizer; the current vectorize
  pass rewrites recognized scalar loops onto existing bulk runtime kernels

### 3. Audit and harden everything that claims to be done

29.8 in particular now looks like "mostly real but not fully proven." That means:
- more optimizer-specific IR regressions
- more object/binary artifact checks
- more real-world reproducers
- living XFAIL canaries for known unfinished or blocked behavior

## Test Surface Sitrep

Current high-level state:
- `131` runtime corpus programs
- `121` programs with `CHECK`
- `10` diagnostic `ERROR_EXPECTED` programs
- `4` programs with `IR_CHECK`
- `5` programs with `IR_NOT`
- `0` living `XFAIL` programs

What is good:
- O0/O1/O2/O3/Ofast runtime matrix is green
- there are explicit determinism regressions for assembly/codegen
- there are targeted IR/capture tests for opt-level-respecting capture, BCE, and GVN
- optimizer unit tests and audit tests are extensive

What is still too weak:
- too few IR-shape assertions relative to the optimizer surface
- almost no object-level assertions
- linked-binary determinism had not been pinned
- no living XFAIL audit canaries are present right now
- 29.9 has almost no direct regression coverage because the passes are not there yet

## Brutal Audit Priorities Inside 29.10

### 1. Artifact determinism and correctness

Audit beyond program output:
- optimized IR shape
- MIR/regalloc shape where useful
- object snapshots
- linked binary reproducibility

Immediate kickoff item:
- add object-level optimization assertions
- pin linked-binary reproducibility
- remove Mach-O UUID noise from linker invocations

### 2. Real-world optimizer reproducers

Add more programs that force:
- cross-block GVN through joins
- SROA on nested aggregates / small arrays / complex values
- BCE on canonical and near-canonical loops
- LSF across calls, diamonds, and loop headers
- LICM legality around aliasing and hidden stores
- loop transform legality (especially interchange/fusion/fission edge cases)

### 3. 29.9 gap audit

Explicitly prove what is absent so we stop hand-waving:
- PURE/ELEMENTAL exploitation
- DO CONCURRENT optimization license
- whole-array vectorization
- IPO passes
- whole-program analysis

## 29.10 Kickoff: 2026-04-09

This cleanup sprint starts with:
1. status consolidation across all `29.x` planning docs
2. runtime-matrix revalidation at every currently supported optimization level
3. artifact-level audit expansion, starting with object and linked-binary determinism

First landed 29.10 slice:
- linker hardening to suppress Mach-O `LC_UUID` noise in compiler and capture link paths
- object snapshot regression proving optimized inlining changes the emitted object
- object determinism regression across opt levels for module-global-heavy code
- linked-binary byte determinism regression for repeated builds at the same output path

Next implementation frontier after this kickoff slice:
1. start with the missing 29.9 work, because that is the largest unfinished body of Sprint 29
2. then burn down the remaining 29.5/29.6/29.7/29.8 cleanup items in descending impact order

The goal is not just "tests pass". The goal is: when Sprint 29 closes, every promised
item is either finished, explicitly deferred with a real dependency, or captured by a
test-backed audit finding.

## Current `i128` Staging Line

Landed so far:
- raw `i128` IR lowering and true wide constant/global storage
- backend emission for `i128` globals
- local `-O0` stack-backed `i128` memory traffic, add/sub/neg, equality, ordered compares,
  and `select`
- internal-only `-O0` pair-register `i128` params, returns, and same-module calls
- external `-O0` pair-register `i128` call/return codegen through real Fortran interface
  declarations, with asm/object determinism coverage
- linked cross-object `-O0` execution against a foreign `__int128` helper object, with
  linked-binary determinism coverage
- stack-passed direct `i128` args for internal and external calls, including incoming
  callee loads from `[x29, #16+]`, outgoing caller stack-area stores, optimized internal
  execution coverage, and linked cross-object determinism coverage against clang-built
  foreign helpers
- full `-O1` optimized-pipeline support for non-global `i128` modules, including mem2reg,
  inlining, dead-arg / const-arg / return propagation, and the ordinary O1 cleanup passes
- `-O1` source-level coverage proving `integer(16)` constant folding can remove unsupported
  wide arithmetic before backend selection, and that branchy mem2reg-promoted `integer(16)`
  locals survive SSA joins correctly, with object determinism and linked cross-object
  execution coverage
- full `-O2` optimized-pipeline support for the current scalar `i128` surface, including
  SROA-adjacent cleanup, LICM/DSE/LSF/GVN-era pass coverage, and linked cross-object
  determinism coverage
- full high-opt (`-O3` / `-Os` / `-Ofast`) optimized-pipeline support for the current
  scalar `i128` surface, including aggressive inlining and loop/vectorization-era pipeline
  coverage where the widened `i128` path stays scalar
- source-level and selector-level regressions with object determinism coverage for the
  supported local `-O0` surface

Still missing inside the same cleanup item:
- broader backend/runtime support for `i128` shapes outside the current scalar surface,
  especially stack-passed wide results, runtime-call `i128`, and more ambitious
  array/vectorization-style wide rewrites if they ever become legal

### Planned ABI Jump

The next large `i128` step should be treated as a dedicated multi-commit tranche, not as
background cleanup mixed into unrelated 29.10 work.

Working assumption from local clang ABI probes on Apple ARM64:
- `__int128` returns use `x0/x1`
- `__int128` params consume register pairs `xN/xN+1`

Staged plan:
1. widen `i128` support beyond the current scalar surface

Timing:
- keep landing bounded `i128` slices while the support boundary is still crisp and testable
- use the full scalar optimized lane as the proving ground before widening to broader
  wide-value surface area
