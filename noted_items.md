# Noted Items

Deferred items that came up while finishing Sprint 29.10 cleanup work:

- Add true formatted `integer(16)` input support. Output formatting is wide now, but
  input still lowers through the simplified list-directed `READ` path rather than a
  real format-driven parser.
- Finish internal character-buffer `integer(16)` I/O. The runtime has narrow
  `afs_read_internal_int` / `afs_write_internal_int` helpers, but lowering still
  does not expose full internal `READ`/`WRITE` for wide integers.
- Add stack-passed wide `integer(16)` results. Wide direct-call args are staged,
  but the broader result ABI surface is still narrower than the call-argument
  surface.
- Audit and widen any remaining `RuntimeCall(..)`-style `integer(16)` runtime entry
  points beyond the current print/write/read/format-push coverage.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
