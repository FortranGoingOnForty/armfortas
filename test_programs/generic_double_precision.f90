! Generic interface dispatch must pick the right specific based on
! BOTH category and kind. Previously the dispatcher only checked
! is_float()/is_int() so every real actual arg was routed to the
! first float specific, mis-interpreting dp bits as f32.
! CHECK: 3
! CHECK: 4.0000000E0
! CHECK: 4.000000000000000E0
module mdp
  implicit none
  interface add
    module procedure add_int, add_real, add_dp
  end interface
contains
  integer function add_int(a, b)
    integer, intent(in) :: a, b
    add_int = a + b
  end function
  real function add_real(a, b)
    real, intent(in) :: a, b
    add_real = a + b
  end function
  double precision function add_dp(a, b)
    double precision, intent(in) :: a, b
    add_dp = a + b
  end function
end module
program t
  use mdp
  implicit none
  integer, parameter :: dp = 8
  real(dp) :: x = 1.5_dp, y = 2.5_dp
  print *, add(1, 2)
  print *, add(1.5, 2.5)
  print *, add(x, y)
end program
