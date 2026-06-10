! l02: F2023 conditional expressions — scalar arms across types,
! right-associative chaining, constant-condition folding, character
! arms with different lengths into a deferred-length allocatable
! (the gfortran conditional_1.f90 shapes).
! FLAGS: --std=f2023
! CHECK: 100
! CHECK: 200
! CHECK: 55
! CHECK: 15
! CHECK: 7
! CHECK: 5 abcde
! CHECK: 2 xy
program l02_conditional_expr
  implicit none
  integer :: i, x
  real :: r
  character(len=:), allocatable :: s
  i = 5
  x = (i > 3 ? 100 : 200)
  print *, x
  x = (i > 9 ? 100 : 200)
  print *, x
  x = (i == 1 ? 1 : i == 5 ? 55 : 99)
  print *, x
  r = (i > 0 ? 1.5 : -1.5)
  print *, int(r * 10)
  x = (.true. ? 7 : 8)
  print *, x
  s = (i /= 0 ? "abcde" : "xy")
  print *, len(s), s
  s = (i == 0 ? "abcde" : "xy")
  print *, len(s), s
end program l02_conditional_expr
