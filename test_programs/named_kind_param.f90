! Named kind parameters: real(c_double), integer(int64), real(real64).
! Verifies that intrinsic module constants are resolved as kind selectors.
! CHECK: 3.14
! CHECK: 9999999999
! CHECK: 2.718281828459045
program t
  use iso_c_binding, only: c_double
  use iso_fortran_env, only: real64, int64
  implicit none
  real(c_double) :: d
  integer(int64) :: i
  real(real64) :: r
  d = 3.14d0
  i = 9999999999_8
  r = 2.718281828459045d0
  print *, d
  print *, i
  print *, r
end program
