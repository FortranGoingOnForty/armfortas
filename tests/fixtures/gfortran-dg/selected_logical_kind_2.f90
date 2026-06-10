! Imported from gcc testsuite gfortran.dg/selected_logical_kind_2.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2018" } + 2x dg-error "has no IMPLICIT type"
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! armfortas already rejects this (SELECTED_LOGICAL_KIND is not an
! intrinsic yet, so under IMPLICIT NONE the name is undeclared) — same
! intent as gfortran's "has no IMPLICIT type". When l04 adds the
! intrinsic, the f2018 conformance gate must keep rejecting this file
! and this expectation will need the gate's wording.
! FLAGS: --std=f2018
! ERROR_EXPECTED: used but not declared
program selected
  implicit none

  logical(selected_logical_kind(1)) :: l ! original dg-error: "has no IMPLICIT type"
  print *, selected_logical_kind(1) ! original dg-error: "has no IMPLICIT type"
end program
