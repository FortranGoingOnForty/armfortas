! Compile-time kind selection intrinsics.
! CHECK: 4 8
! CHECK: 999999999
! CHECK: 3.141592653589793
program t
  implicit none
  integer, parameter :: i4 = selected_int_kind(9)
  integer, parameter :: r8 = selected_real_kind(15)
  integer(i4) :: n
  real(r8) :: x
  n = 999999999
  x = 3.141592653589793d0
  print *, i4, r8
  print *, n
  print *, x
end program
