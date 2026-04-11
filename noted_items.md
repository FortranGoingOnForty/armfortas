# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work:

- Add true formatted `integer(16)` input support. Output formatting and list-directed
  reads are wide now, but input still lowers through the simplified list-directed
  `READ` path rather than a real format-driven parser.
- Finish formatted/internal-input `integer(16)` support. List-directed character-buffer
  `READ`/`WRITE` and formatted internal `WRITE` are landed, but formatted internal
  `READ` still does not have a real parser-backed path.
- Add stack-passed wide `integer(16)` results. Wide direct-call args are staged,
  but the broader result ABI surface is still narrower than the call-argument
  surface.
- Audit and widen any remaining `RuntimeCall(..)`-style `integer(16)` runtime entry
  points beyond the current print/write/read/internal-write/internal-read/format-push
  coverage.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
