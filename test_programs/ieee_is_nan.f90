! Test ieee_is_nan and ieee_is_finite from ieee_arithmetic.
! CHECK: F
! CHECK: T
! CHECK: T
! CHECK: F
program t
  use ieee_arithmetic, only: ieee_is_nan, ieee_is_finite
  implicit none
  real :: x, nan_val, inf_val

  x = 1.0
  nan_val = 0.0
  nan_val = nan_val / nan_val    ! 0/0 = NaN
  inf_val = 1.0
  inf_val = inf_val / 0.0       ! 1/0 = Inf

  ! Normal value: not NaN, is finite
  print *, ieee_is_nan(x)       ! F
  print *, ieee_is_finite(x)    ! T

  ! NaN: is NaN, not finite
  print *, ieee_is_nan(nan_val)    ! T
  print *, ieee_is_finite(nan_val) ! F
end program
