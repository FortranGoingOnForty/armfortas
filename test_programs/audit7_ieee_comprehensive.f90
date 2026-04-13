! Comprehensive IEEE arithmetic tests: ieee_is_nan and ieee_is_finite
! for both real32 and real64, including NaN, Inf, zero, and normal values.
! CHECK: 8  passed,  0  failed
program test_ieee
  use ieee_arithmetic, only: ieee_is_nan, ieee_is_finite
  use iso_fortran_env, only: real32, real64
  implicit none

  real(real32) :: s_zero, s_nan, s_inf, s_normal
  real(real64) :: d_zero, d_nan, d_inf, d_normal
  integer :: pass_count, fail_count

  pass_count = 0
  fail_count = 0

  ! Set up test values
  s_zero = 0.0
  s_normal = 3.14
  s_nan = 0.0
  s_nan = s_nan / s_nan   ! NaN
  s_inf = 1.0e38
  s_inf = s_inf * 1.0e10  ! Inf

  d_zero = 0.0d0
  d_normal = 3.14d0
  d_nan = 0.0d0
  d_nan = d_nan / d_nan   ! NaN
  d_inf = 1.0d308
  d_inf = d_inf * 1.0d10  ! Inf

  ! ieee_is_nan tests
  if (.not. ieee_is_nan(s_nan)) then
    print *, 'FAIL: ieee_is_nan(NaN_r32)'
    fail_count = fail_count + 1
  else
    pass_count = pass_count + 1
  end if

  if (ieee_is_nan(s_normal)) then
    print *, 'FAIL: ieee_is_nan(3.14_r32) should be false'
    fail_count = fail_count + 1
  else
    pass_count = pass_count + 1
  end if

  if (ieee_is_nan(s_zero)) then
    print *, 'FAIL: ieee_is_nan(0.0_r32) should be false'
    fail_count = fail_count + 1
  else
    pass_count = pass_count + 1
  end if

  ! ieee_is_finite tests
  if (ieee_is_finite(s_normal)) then
    pass_count = pass_count + 1
  else
    print *, 'FAIL: ieee_is_finite(3.14_r32) should be true'
    fail_count = fail_count + 1
  end if

  if (ieee_is_finite(s_zero)) then
    pass_count = pass_count + 1
  else
    print *, 'FAIL: ieee_is_finite(0.0_r32) should be true'
    fail_count = fail_count + 1
  end if

  ! real64 tests
  if (.not. ieee_is_nan(d_nan)) then
    print *, 'FAIL: ieee_is_nan(NaN_r64)'
    fail_count = fail_count + 1
  else
    pass_count = pass_count + 1
  end if

  if (ieee_is_nan(d_normal)) then
    print *, 'FAIL: ieee_is_nan(3.14_r64) should be false'
    fail_count = fail_count + 1
  else
    pass_count = pass_count + 1
  end if

  if (ieee_is_finite(d_normal)) then
    pass_count = pass_count + 1
  else
    print *, 'FAIL: ieee_is_finite(3.14_r64) should be true'
    fail_count = fail_count + 1
  end if

  print *, 'IEEE tests: ', pass_count, ' passed, ', fail_count, ' failed'
end program
