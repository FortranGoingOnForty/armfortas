# armfortas tests

The root `tests/` directory now holds armfortas-local harnesses and integration
checks that belong in the compiler repo itself.

The extracted structured bench lives in the `bencch/` submodule.

Current handoff point:

- `bencch` was split out after the Sprint 6 audit/hardening slice.
- The next planned bench slice is deeper Sprint 6 differential coverage and
  object/tool consistency work.
