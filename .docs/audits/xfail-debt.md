# XFAIL debt registry

Every active XFAIL parsed by the root end-to-end harness must cite one of these
stable IDs or an `X64-O0-NNN` finding from `x86_64-o0-sweep.md`. This covers
both `test_programs/` and imported compatibility fixtures. Remove the annotation
when the underlying condition is fixed; retain resolved entries here with
`[FIXED]` in the heading when historical context remains useful.

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

## XFAIL-003 - Whole-array bounds intrinsics are not lowered

**Status:** Active compiler defect.

The no-`dim` forms of `LBOUND` and `UBOUND` are left as unresolved external
symbols instead of being lowered for arrays. The imported `C_F_POINTER` fixture
exposes the general intrinsic gap while checking pointer lower bounds.

## XFAIL-004 - Conditional diagnostics do not match imported oracles

**Status:** Active diagnostic compatibility debt.

Conditional-expression syntax and resolution failures are rejected before the
compiler can emit the diagnostic substrings expected by the imported gfortran
fixtures. The fixtures remain expected failures until their error oracle can be
matched without relying on source-line echoing.

## XFAIL-005 - Duplicate DO CONCURRENT locality is accepted

**Status:** Active compiler defect.

Semantic validation does not reject a variable that appears in both `SHARED`
and `REDUCE` locality specifications on the same `DO CONCURRENT` construct.
