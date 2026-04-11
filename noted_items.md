# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work:

- Broaden formatted `integer(16)` input coverage beyond scalar lvalues.
  Top-level scalars, array elements, and derived-type components now have real
  parser-backed internal/external `READ` paths, but sections, whole-array
  destinations, and richer non-scalar formatted input still need honest wide
  support.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
