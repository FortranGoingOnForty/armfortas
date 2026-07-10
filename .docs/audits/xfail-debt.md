# XFAIL debt registry

Every active `test_programs/` XFAIL must cite one of these stable IDs or an
`X64-O0-NNN` finding from `x86_64-o0-sweep.md`. Remove the annotation when the
underlying condition is fixed; retain resolved entries here with `[FIXED]` in
the heading when historical context remains useful.

## XFAIL-001 - macOS rejects non-UTF-8 filename bytes

**Status:** Active platform limitation.

Darwin rejects a filename containing raw byte `0xe9` before armfortas runtime
I/O begins. The byte-preservation test remains active on other hosts and is
expected to fail only on macOS. The original platform qualification is recorded
in `.docs/audits/x86-campaign-log.md`.

## XFAIL-002 - PURE host-associated allocation is not rejected

**Status:** Active compiler defect.

Semantic validation does not yet reject `ALLOCATE` or `DEALLOCATE` in a PURE
procedure when the affected allocatable is host-associated, as required by
Fortran 2018 section 15.7. The paired diagnostic fixtures cover both statements.
