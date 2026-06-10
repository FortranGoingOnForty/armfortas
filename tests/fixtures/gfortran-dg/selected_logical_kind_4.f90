! Imported from gcc testsuite gfortran.dg/selected_logical_kind_4.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do run } (gfortran default flags)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! FLAGS: --std=f2023
! EXIT_CODE: 0
! XFAIL: f2023 SELECTED_LOGICAL_KIND not implemented (l04); see .docs/audits/f2023-feature-matrix.md
! Check that SELECTED_LOGICAL_KIND works in a non-constant context
! (which is rare but allowed)

subroutine foo(i, j)
  implicit none
  integer :: i, j
  if (selected_logical_kind(i) /= j) STOP j
end subroutine

program selected
  implicit none

  call foo(1, 1)
  call foo(8, 1)
  call foo(9, 2)
  call foo(16, 2)
  call foo(17, 4)
  call foo(32, 4)
  call foo(33, 8)
  call foo(64, 8)
end program
