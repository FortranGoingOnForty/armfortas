! Imported from gcc testsuite gfortran.dg/conditional_2.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do run } { dg-options "-std=f2023" }
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! FLAGS: --std=f2023
! EXIT_CODE: 0
! XFAIL: f2023 conditional expressions not implemented (l02); see .docs/audits/f2023-feature-matrix.md
program conditional_constant
  implicit none
  integer :: i = 42

  print *, (.true. ? 1 : -1)
  print *, (.false. ? "hello" : "world")
  i = (.true. ? 1 : -1)
  if (i /= 1) stop 1

  i = 0
  i = (i > 0 ? 1 : .false. ? -1 : 0)
  if (i /= 0) stop 2
end program conditional_constant
