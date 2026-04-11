# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work:

- Broaden formatted `integer(16)` input coverage. Top-level scalar formatted
  internal/external `READ` now have real parser-backed paths, but richer
  descriptor coverage and broader non-scalar formatted input still need honest
  wide support.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
