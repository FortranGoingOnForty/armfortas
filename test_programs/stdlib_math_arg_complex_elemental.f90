! CHECK: ok
! IR_CHECK: call @atan2(
! IR_CHECK: cmplx_array_check
! REPRO_CHECK: run
program stdlib_math_arg_complex_elemental
  implicit none
  integer, parameter :: sp = kind(1.0)
  integer, parameter :: dp = kind(1.0d0)
  real(sp), parameter :: pi_sp = acos(-1.0_sp)
  real(dp), parameter :: pi_dp = acos(-1.0_dp)
  real(sp), parameter :: tol_sp = sqrt(epsilon(1.0_sp))
  real(dp), parameter :: tol_dp = sqrt(epsilon(1.0_dp))
  real(sp) :: theta_sp(3), got_sp(3), expect_sp(3)
  real(dp) :: theta_dp(3), got_dp(3), expect_dp(3)
  complex(dp) :: z_dp

  z_dp = 2.0_dp * exp((0.0_dp, 0.5_dp))
  if (abs(atan2(z_dp%im, z_dp%re) - 0.5_dp) >= tol_dp) error stop 1
  if (abs(argd_dp((-1.0_dp, 0.0_dp)) - 180.0_dp) >= tol_dp) error stop 2
  if (abs(argpi_dp((-1.0_dp, 0.0_dp)) - 1.0_dp) >= tol_dp) error stop 3

  theta_sp = [-179.0_sp, 0.0_sp, 179.0_sp]
  got_sp = arg_sp(exp(cmplx(0.0_sp, theta_sp / 180.0_sp * pi_sp, kind=sp)))
  expect_sp = theta_sp / 180.0_sp * pi_sp
  if (.not. all(abs(got_sp - expect_sp) < tol_sp)) error stop 4

  theta_dp = [-179.0_dp, 0.0_dp, 179.0_dp]
  got_dp = arg_dp(exp(cmplx(0.0_dp, theta_dp / 180.0_dp * pi_dp, kind=dp)))
  expect_dp = theta_dp / 180.0_dp * pi_dp
  if (.not. all(abs(got_dp - expect_dp) < tol_dp)) error stop 5

  print *, 'ok'

contains
  elemental function arg_sp(z) result(result)
    complex(sp), intent(in) :: z
    real(sp) :: result

    result = merge(0.0_sp, atan2(z%im, z%re), z == (0.0_sp, 0.0_sp))
  end function

  elemental function arg_dp(z) result(result)
    complex(dp), intent(in) :: z
    real(dp) :: result

    result = merge(0.0_dp, atan2(z%im, z%re), z == (0.0_dp, 0.0_dp))
  end function

  elemental function argd_dp(z) result(result)
    complex(dp), intent(in) :: z
    real(dp) :: result

    result = merge(0.0_dp, atan2(z%im, z%re) * 180.0_dp / pi_dp, z == (0.0_dp, 0.0_dp))
  end function

  elemental function argpi_dp(z) result(result)
    complex(dp), intent(in) :: z
    real(dp) :: result

    result = merge(0.0_dp, atan2(z%im, z%re) / pi_dp, z == (0.0_dp, 0.0_dp))
  end function
end program
