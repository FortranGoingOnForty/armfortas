# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work:

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
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
