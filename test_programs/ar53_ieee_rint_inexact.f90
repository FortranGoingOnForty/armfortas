! IEEE_RINT(X, ROUND) is the quiet roundToIntegral{rounding} operation and
! must not raise IEEE_INEXACT. IEEE_RINT(X) is roundToIntegralExact and must
! raise the flag when finite X changes. Flags remain sticky in both forms.
!
! FLAGS: --std=f2023
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_ieee_rint_r8_round(
! IR_CHECK: call @afs_ieee_rint_r4_round(
program ar53_ieee_rint_inexact
  use iso_fortran_env, only: real32, real64
  use ieee_arithmetic
  use ieee_exceptions, only: ieee_get_flag, ieee_inexact, ieee_set_flag
  implicit none

  type(ieee_round_type) :: saved_mode, current_mode
  real(real64) :: x64
  real(real32) :: x32
  logical :: saved_inexact, raised
  integer :: opaque_zero

  opaque_zero = command_argument_count()
  call ieee_get_rounding_mode(saved_mode)
  call ieee_get_flag(ieee_inexact, saved_inexact)
  call ieee_set_rounding_mode(ieee_down)

  call ieee_set_flag(ieee_inexact, .false.)
  x64 = ieee_rint(1.1_real64 + real(opaque_zero, real64), ieee_up)
  call ieee_get_flag(ieee_inexact, raised)
  if (x64 /= 2.0_real64) error stop 1
  if (raised) error stop 2
  call ieee_get_rounding_mode(current_mode)
  if (current_mode /= ieee_down) error stop 3

  call ieee_set_flag(ieee_inexact, .false.)
  x32 = ieee_rint(-1.1_real32 + real(opaque_zero, real32), ieee_down)
  call ieee_get_flag(ieee_inexact, raised)
  if (x32 /= -2.0_real32) error stop 4
  if (raised) error stop 5

  call ieee_set_flag(ieee_inexact, .false.)
  x64 = ieee_rint(2.0_real64 + real(opaque_zero, real64), ieee_nearest)
  call ieee_get_flag(ieee_inexact, raised)
  if (x64 /= 2.0_real64) error stop 6
  if (raised) error stop 7

  x64 = ieee_rint(ieee_value(0.0_real64, ieee_positive_inf), ieee_down)
  call ieee_get_flag(ieee_inexact, raised)
  if (x64 <= huge(0.0_real64)) error stop 8
  if (raised) error stop 9

  x32 = ieee_rint(ieee_value(0.0_real32, ieee_quiet_nan), ieee_up)
  call ieee_get_flag(ieee_inexact, raised)
  if (.not. ieee_is_nan(x32)) error stop 10
  if (raised) error stop 11

  call ieee_set_flag(ieee_inexact, .true.)
  x32 = ieee_rint(3.0_real32 + real(opaque_zero, real32), ieee_nearest)
  call ieee_get_flag(ieee_inexact, raised)
  if (x32 /= 3.0_real32) error stop 12
  if (.not. raised) error stop 13

  call ieee_set_flag(ieee_inexact, .false.)
  x64 = ieee_rint(1.9_real64 + real(opaque_zero, real64))
  call ieee_get_flag(ieee_inexact, raised)
  if (x64 /= 1.0_real64) error stop 14
  if (.not. raised) error stop 15

  call ieee_set_rounding_mode(saved_mode)
  call ieee_set_flag(ieee_inexact, saved_inexact)
  print '(a)', 'ok'
end program ar53_ieee_rint_inexact
