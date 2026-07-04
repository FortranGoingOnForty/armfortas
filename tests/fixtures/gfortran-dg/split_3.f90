! Imported from gcc testsuite gfortran.dg/split_3.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do run } { dg-shouldfail "Fortran runtime error" } (POS out of range)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Expected runtime failure (POS out of range). EXIT_CODE 1 follows the
! armfortas runtime-error convention; afs_split bounds-checks POS
! per 16.9.197 (L-tail, 2026-07-04).
! FLAGS: --std=f2023
! EXIT_CODE: 1
program b
  character(len=:), allocatable :: input
  character(len=2) :: set = ', '
  integer :: p
  input = " one,last example,"
  p = -1
  call split(input, set, p)
end program b
