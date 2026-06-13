! Imported from gcc testsuite gfortran.dg/selected_logical_kind_2.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2018" } + 2x dg-error "has no IMPLICIT type"
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! gfortran rejects this under -std=f2018 ("has no IMPLICIT type") because
! SELECTED_LOGICAL_KIND is an F2023 intrinsic. l04 implements it and
! gates it behind --std=f2023, so the f2018 conformance gate keeps
! rejecting this file — now with the gate's own wording.
! FLAGS: --std=f2018
! ERROR_EXPECTED: SELECTED_LOGICAL_KIND requires --std=F2023
program selected
  implicit none

  logical(selected_logical_kind(1)) :: l ! original dg-error: "has no IMPLICIT type"
  print *, selected_logical_kind(1) ! original dg-error: "has no IMPLICIT type"
end program
