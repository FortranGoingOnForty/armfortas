! Imported from gcc testsuite gfortran.dg/split_1.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do run } (gfortran default flags)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! FLAGS: --std=f2023
! EXIT_CODE: 0
program b
  character(len=:), allocatable :: input
  character(len=2) :: set = ', '
  integer :: p
  input = " one,last example,"
  p = 0

  call split(input, set, p)
  if (p /= 1) STOP 1
  call split(input, set, p)
  if (p /= 5) STOP 2
  call split(input, set, p)
  if (p /= 10) STOP 3
  call split(input, set, p)
  if (p /= 18) STOP 4
  call split(input, set, p)
  if (p /= 19) STOP 5

  call split(input, set, p, .true.)
  if (p /= 18) STOP 6
  call split(input, set, p, .true.)
  if (p /= 10) STOP 7
  call split(input, set, p, .true.)
  if (p /= 5) STOP 8
  call split(input, set, p, .true.)
  if (p /= 1) STOP 9
end program b
