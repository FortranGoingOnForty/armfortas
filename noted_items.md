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
- Parser gap: typed character array constructors using an explicit type-spec
  inside brackets (for example `[character(len=20) :: "a", "b"]`) still fail
  to parse in at least one real-world-style source shape.
  The full-sprint audit tripped this while building the fpm-inspired
  `realworld_suffix_scan.f90` reproducer, which is currently written in a more
  conservative source form instead of the typed constructor spelling.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
