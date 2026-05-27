! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program complex_zero_integer_power_lapack_shift
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none

  integer, parameter :: ilp = 4

  call check_sp()
  call check_dp()
  print *, "ok"

contains
  subroutine check_sp()
    complex(real32) :: x, u, y, y_default, t
    real(real32) :: s

    x = (0.0_real32, 0.0_real32)
    u = (1.0_real32, 0.0_real32)
    s = 1.0_real32

    y = s * sqrt((x / s)**2_ilp + (u / s)**2_ilp)
    y_default = s * sqrt((x / s)**2 + (u / s)**2)
    t = (1.0_real32, 0.0_real32) - u * (u / (x + y))

    if (abs(real(y) - 1.0_real32) > 1.0e-6_real32) error stop 11
    if (abs(aimag(y)) > 1.0e-6_real32) error stop 12
    if (abs(real(y_default) - 1.0_real32) > 1.0e-6_real32) error stop 13
    if (abs(aimag(y_default)) > 1.0e-6_real32) error stop 14
    if (abs(real(t)) > 1.0e-6_real32) error stop 15
    if (abs(aimag(t)) > 1.0e-6_real32) error stop 16
  end subroutine check_sp

  subroutine check_dp()
    complex(real64) :: x, u, y, y_default, t
    real(real64) :: s

    x = (0.0_real64, 0.0_real64)
    u = (1.0_real64, 0.0_real64)
    s = 1.0_real64

    y = s * sqrt((x / s)**2_ilp + (u / s)**2_ilp)
    y_default = s * sqrt((x / s)**2 + (u / s)**2)
    t = (1.0_real64, 0.0_real64) - u * (u / (x + y))

    if (abs(real(y) - 1.0_real64) > 1.0e-12_real64) error stop 21
    if (abs(aimag(y)) > 1.0e-12_real64) error stop 22
    if (abs(real(y_default) - 1.0_real64) > 1.0e-12_real64) error stop 23
    if (abs(aimag(y_default)) > 1.0e-12_real64) error stop 24
    if (abs(real(t)) > 1.0e-12_real64) error stop 25
    if (abs(aimag(t)) > 1.0e-12_real64) error stop 26
  end subroutine check_dp
end program complex_zero_integer_power_lapack_shift
