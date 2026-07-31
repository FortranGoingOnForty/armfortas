! STDERR_CHECK: ERROR STOP
! EXIT_CODE: 1
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program inquire_iolength_zero_step_runtime
  use, intrinsic :: iso_fortran_env, only: int64
  implicit none

  integer(int64) :: j, step
  integer :: n

  step = 0_int64
  inquire(iolength=n) (j, j=1_int64, 2_int64, step)
  print '(i0)', n
end program inquire_iolength_zero_step_runtime
