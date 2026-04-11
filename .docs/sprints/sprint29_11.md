# Sprint 29.11: Full Sprint 29 Audit

## Why This Sprint Exists

Sprint 29 no longer needs another cleanup/build-out sub-sprint. It needs an
exhaustive audit.

29.10 closed the hidden implementation gaps that were still preventing an honest
"mostly done" story. What remains now is proving that the optimizer surface we
claim to have is actually correct:
- across IR, assembly, objects, linked binaries, and runtime behavior
- across `-O0/-O1/-O2/-O3/-Os/-Ofast`
- across real-world-style Fortran programs, not just isolated micro-reproducers

This sprint is the formal closeout audit for all of Sprint 29, including
29.5-29.10.

## Audit Scope

Sources of truth:
- [Sprint 29](sprint29.md)
- [Sprint 29.5](sprint29_5.md)
- [Sprint 29.6](sprint29_6.md)
- [Sprint 29.7](sprint29_7.md)
- [Sprint 29.8](sprint29_8.md)
- [Sprint 29.9](sprint29_9.md)
- [Sprint 29.10](sprint29_10.md)

Each promised item should end the audit in one of three states:
1. proven working with living tests
2. explicitly deferred with a written dependency
3. captured by a living XFAIL or audit regression

## Audit Rules

- Prefer real Fortran programs in `test_programs/` over only unit-level IR toys.
- Every new audit program should try to prove more than stdout:
  - runtime result correctness
  - IR/asm/object presence where relevant
  - object or linked-binary determinism where relevant
  - cross-opt equality unless the test is intentionally opt-sensitive
- Use `.refs` as the sanity-check source for hotspot shapes and realistic coding
  patterns, especially stdlib/fpm-style loops, source scanners, and numerical kernels.
- When the audit finds a real bug, land the smallest honest fix plus a regression.
- When the audit finds a real bug that is not fixed immediately, record it in
  `noted_items.md` and capture it as a living `XFAIL` where possible.

## Initial Tranche

Kickoff items:
- source-level audit for helper-before-program entry lowering / dead-function root handling
- living XFAIL for module procedure host-association over module globals
- real-world stdlib-style kernel:
  - tridiagonal sparse matvec (`realworld_tridiag_spmv.f90`)
- real-world BLAS-style kernel:
  - axpy + reduction (`realworld_axpy_reduce.f90`)
- real-world fpm-style application logic:
  - source suffix classification (`realworld_suffix_scan.f90`)

The passing real-world programs should prove:
- runtime correctness
- phase triangulation (`ir|asm|obj|repro`)
- cross-opt equality
- deterministic object snapshots
- deterministic linked binaries without Mach-O UUID noise

Current audit findings:
- fixed: helper-before-program lowering / dead-function rooting could drop the
  true `__prog_*` entry or make `_main` call the wrong helper first
- fixed: named-parameter local fixed-array extents were folded as `(1, 1)` in
  lowering, which made real-world kernels like `realworld_axpy_reduce.f90` and
  `realworld_tridiag_spmv.f90` trip bogus bounds checks
- fixed: ordinary load-bearing loops were entering an unsafe full-unroll path,
  which miscompiled `realworld_axpy_reduce.f90` at `-O2/-O3/-Ofast`; the
  unroller is now hardened to keep that shape out while preserving the proven
  `DO CONCURRENT` full-unroll path
- fixed: BCE only recognized the canonical bare loop IV, so real-world counted
  loops with safe `iv +/- const` array accesses kept redundant bounds checks at
  `-O2+`; the audit kernels `realworld_sasum_cleanup.f90` and
  `realworld_three_point_apply.f90` now prove the offset-IV case and keep SROA
  honest at the same time
- fixed: cross-block LSF treated every call as a universal memory clobber, so
  branch-join reuse through a noalias helper side path stayed as a reload at
  `-O2+`; `realworld_noalias_reuse.f90` now proves the noalias-call case and
  also keeps the local same-block reuse path honest
- fixed: contained procedures only partially inherited host-associated
  `parameter` constants during lowering, so dummy array extents and loop bounds
  like `x(n)`, `y(n)`, and `do i = 1, n` could degrade inside real-world helper
  kernels; `realworld_seed_overwrite.f90` now proves that host-param-backed
  dummy extents and loop bounds stay intact
- fixed: backend `ICmp` lowering could emit mixed-width GP compares like
  `cmp w26, x23` when the IR compared a 32-bit induction value against a 64-bit
  bound; `realworld_ipo_chain.f90` now keeps the compare-width harmonization
  honest through a real helper-chain compile at `-O2+`
- fixed: module procedures were still missing host association over their own
  module globals, so small cases like `call bump()` could silently leave a
  shared module variable unchanged. `module_global_host_assoc.f90` is now a
  passing audit program with cross-opt equality plus asm/object/run reproducibility,
  and `tests/module_host_audit.rs` proves the raw IR resolves the shared module
  global inside the procedure body
- fixed: extended `OPEN` lowering built the runtime control block by storing
  typed fields through a byte-pointer GEP, which first tripped IR verification
  for `position='append'` and then, after the verifier fix, still wrote fields
  at scaled-by-element-size offsets. `io_append_log.f90` is now a passing file
  oracle with append rerun coverage plus asm/object reproducibility and
  cross-opt equality
- fixed: descriptor-backed array query intrinsics (`SIZE`, `LBOUND`, `UBOUND`)
  were lowered as raw `i64` runtime results even though Fortran default integer
  queries should materialize as default-kind scalars, and scalar/component
  assignment lowering skipped mixed-width coercion at ordinary store sites;
  `realworld_shape_guard.f90` now proves the default-kind runtime-shape path
  through real allocatable metadata, loop bounds, and deterministic objects
- fixed: backend `MovReg` emission did not handle `x -> w` truncation views, so
  real-world default-kind array-query assignments could produce invalid asm like
  `mov w21, x20`; the new runtime-shape audit keeps that truncation surface
  honest
- proven: LICM hoists invariant scalar dummy loads out of a real-world affine
  update loop in `realworld_affine_shift.f90` once BCE clears the loop body
- proven: GVN reduces duplicated branch-join PURE helper calls in
  `realworld_join_bias_sum.f90` at `-O2+` instead of recomputing the same affine
  helper result through the join
- proven: DSE removes the dead seed store in `realworld_seed_overwrite.f90`
  across the intervening noalias helper call while preserving the real fill
- proven: SROA scalarizes the fixed tap buffer in `realworld_binomial_blend.f90`
  and BCE clears the corresponding safe stencil bounds checks at `-O2+`, giving
  us another living real-world audit kernel for the small-aggregate path
- proven: loop-legality audit kernels `realworld_inplace_prefix.f90` and
  `realworld_inplace_symmix.f90` stay runtime-correct, cross-opt-equal, and
  deterministic across IR/object/binary surfaces
- proven: the 29.9 single-file story now has real-world audit coverage for
  ELEMENTAL lowering plus DO CONCURRENT bulk redirection
  (`realworld_elemental_stage.f90`), intramodule IPO helper trimming
  (`realworld_ipo_chain.f90`), small-loop DO CONCURRENT exploitation
  (`realworld_doconc_square.f90`), and explicit-DO vectorization onto the bulk
  runtime kernels (`realworld_vector_stage.f90`)
- deferred with living XFAIL: `FPM-SUFFIX-1` fpm-style source suffix scan still
  fails after parsing with an `i32`/`i64` IR store mismatch
- deferred with living XFAIL: `ASHAPE-SIZE-1` dummy-array `SIZE(...)` lowering
  still routes dummy arrays into the descriptor runtime path even though ordinary
  dummy arrays are carried as base pointers today. The default-integer query
  typing bug is now fixed, so the remaining failure is the real descriptor
  mismatch: `realworld_assumed_shape_size.f90` reaches `afs_array_size` with
  bogus dummy-array metadata and panics in the runtime
- separately deferred parser gap: typed character array constructors using an
  explicit type-spec inside `[]`

Current audit corpus snapshot:
- `183` top-level `test_programs/*.f90` runtime corpus programs
- `172` programs with `CHECK`
- `30` programs with `IR_CHECK`
- `6` programs with `IR_NOT`
- `2` living `XFAIL`s

## Brutal Audit Priorities

### 1. 29.8 Optimizer Proof

Grow adversarial real-world coverage for:
- GVN
- SROA
- BCE
- local and cross-block LSF
- LICM
- loop legality transforms

### 2. 29.9 Claims Audit

Prove the current single-file story honestly:
- PURE/ELEMENTAL exploitation
- DO CONCURRENT exploitation
- intramodule IPO
- vectorization/runtime-kernel redirection

Also keep proving what is still absent:
- cross-module IPO
- whole-program analysis
- general native vectorizer

### 3. Binary Correctness & Determinism

For representative real-world programs:
- object snapshots deterministic at optimized levels
- linked binaries byte-identical when rebuilt at the same output path
- no `LC_UUID`
- runtime behavior equal across optimization levels unless explicitly exempted

## Success Condition

Sprint 29 closes when:
- the promised optimizer/runtime surface has living proof
- the remaining holes are few, explicit, and written down
- the test suite gives us confidence in IR, binary, determinism, and integration
  rather than only stdout
