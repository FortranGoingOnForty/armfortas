! CHECK: 3FF0000000000001
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program fpenv_callee_constant_fold
  use, intrinsic :: ieee_arithmetic
  use iso_fortran_env, only: real64, int64
  implicit none
  real(real64) :: rounded_up

  call ieee_set_rounding_mode(ieee_up)
  rounded_up = add_half_ulp()
  call ieee_set_rounding_mode(ieee_nearest)
  print '(z16.16)', transfer(rounded_up, 0_int64)
contains
  function add_half_ulp() result(value)
    real(real64) :: value

    value = 1.0_real64 + 5.5511151231257827e-17_real64
  end function add_half_ulp
end program fpenv_callee_constant_fold
