! FLAGS: -fcheck=bounds
! STDERR_CHECK: Bounds check failed: index -127 outside [1, 200]
! EXIT_CODE: 1
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program bounds_narrowing_cast
  use iso_fortran_env, only: int8
  implicit none
  integer :: a(200), i

  a = 0
  do i = 129, 130
    a(int(i, int8)) = 7
  end do
  print '(a)', 'survived'
end program bounds_narrowing_cast
