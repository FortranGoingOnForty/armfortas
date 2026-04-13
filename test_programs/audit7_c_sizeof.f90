! c_sizeof intrinsic: returns byte size of C-interoperable types.
! CHECK: sizeof(double)= 8
! CHECK: sizeof(int)= 4
program test_c_sizeof
  use iso_c_binding, only: c_sizeof, c_double, c_int
  implicit none

  real(c_double) :: d
  integer(c_int) :: i

  print *, 'sizeof(double)=', c_sizeof(d)
  print *, 'sizeof(int)=', c_sizeof(i)
end program
