! Mixed-width integer arithmetic: int64 + int32 with implicit widening.
! Regression test for mixed-width register orn and integer promotion.
! CHECK: n= 101
program test_mixed_int
  use iso_fortran_env, only: int64
  implicit none

  integer(int64) :: n
  integer :: small

  n = 100
  small = 1
  n = n + small
  print *, 'n=', n
end program
