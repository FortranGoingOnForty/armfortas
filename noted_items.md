# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work:

- Broaden formatted `integer(16)` input coverage. Top-level scalar formatted
  internal/external `READ` now have real parser-backed paths, but richer
  descriptor coverage and broader non-scalar formatted input still need honest
  wide support.
- Add stack-passed wide `integer(16)` results. Wide direct-call args are staged,
  but the broader result ABI surface is still narrower than the call-argument
  surface.
- Audit and widen any remaining `RuntimeCall(..)`-style `integer(16)` runtime entry
  points beyond the current raw-IR `PrintInt`/print/write/read/internal-write/
  internal-read/format-push/format-read coverage.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
