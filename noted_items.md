# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work:

- Broaden formatted `integer(16)` input coverage beyond today's landed lvalues.
  Top-level scalars, array elements, derived-type components, whole-array
  destinations, 1-D slices, and fixed-shape multi-dimensional sections now have
  real parser-backed internal/external `READ` paths, but richer non-scalar
  formatted input still needs honest wide support.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
