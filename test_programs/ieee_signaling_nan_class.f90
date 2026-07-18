! IEEE_VALUE must preserve the distinction between signaling and quiet NaNs.
! CHECK: real64 T F T
! CHECK: real32 T F T
program ieee_signaling_nan_class
  use, intrinsic :: ieee_arithmetic, only: ieee_class, ieee_is_nan, &
       ieee_quiet_nan, ieee_signaling_nan, ieee_value
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none
  real(real64) :: x64
  real(real32) :: x32

  x64 = ieee_value(0.0_real64, ieee_signaling_nan)
  x32 = ieee_value(0.0_real32, ieee_signaling_nan)

  print *, 'real64', ieee_class(x64) == ieee_signaling_nan, &
                      ieee_class(x64) == ieee_quiet_nan, ieee_is_nan(x64)
  print *, 'real32', ieee_class(x32) == ieee_signaling_nan, &
                      ieee_class(x32) == ieee_quiet_nan, ieee_is_nan(x32)
end program ieee_signaling_nan_class
