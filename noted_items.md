# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work and
starting the full Sprint 29 audit:

- Audit and harden descriptor-backed `integer(16)` formatted section reads at the
  backend/harness boundary.
  The lowering gap is now closed for allocatable section destinations with real
  runtime bounds/strides, and dedicated fixture-backed audits cover IR plus
  O1+/high-opt runtime behavior, but O0 still exposes the existing large-frame
  slot-addressing backend issue in some cases and the external whole-array
  fixture still returns `exit -1` under `capture_from_path(...Stage::Run...)`
  even though direct CLI O1/O2/O3 runs succeed.
- Audit MODULE-HOST-1: module procedures currently miss module-global host
  association in at least one small reproducer.
  `test_programs/module_global_host_assoc.f90` now captures the current behavior
  as a living XFAIL after the brutal audit uncovered that `call bump()` leaves
  the module variable unchanged instead of writing `99`.
- Parser gap: typed character array constructors using an explicit type-spec
  inside brackets (for example `[character(len=20) :: "a", "b"]`) still fail
  to parse in at least one real-world-style source shape.
  The full-sprint audit tripped this while building the fpm-inspired
  `realworld_suffix_scan.f90` reproducer, which is currently written in a more
  conservative source form instead of the typed constructor spelling.
- Audit FPM-SUFFIX-1: the conservative `realworld_suffix_scan.f90` variant now
  gets past parsing but still fails IR verification with an `i32`/`i64` store
  type mismatch.
  This is now a living XFAIL in the real-world audit corpus, so the Sprint 29
  closeout has a standing canary for the remaining source-scanner gap.
- Audit ASHAPE-SIZE-1: dummy-array `SIZE(...)` queries still flow into the
  descriptor runtime path even though ordinary dummy arrays are currently
  lowered as base pointers rather than descriptors.
  The 29.11 claims audit surfaced this first through the original
  `realworld_ipo_chain.f90` helper loop, and
  `test_programs/realworld_assumed_shape_size.f90` now captures it as a living
  XFAIL canary instead of letting the finding disappear.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
