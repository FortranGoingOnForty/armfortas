! IEEE_SUPPORT_ROUNDING must describe the modes the hardware rounding-mode
! setter can represent. Binary IEEE targets support nearest, toward zero,
! upward, and downward; nearest-away and the sentinel OTHER are not global
! hardware modes on the supported ARM64 and x86-64 targets.
!
! FLAGS: --std=f2023
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_ieee_support_rounding(
program ar53_ieee_rounding_support
  use, intrinsic :: ieee_arithmetic
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none

  type(ieee_round_type) :: requested
  real(real32) :: x32
  real(real64) :: x64

  requested = ieee_nearest
  if (.not. ieee_support_rounding(requested)) error stop 1
  requested = ieee_to_zero
  if (.not. ieee_support_rounding(requested)) error stop 2
  requested = ieee_up
  if (.not. ieee_support_rounding(requested, x32)) error stop 3
  requested = ieee_down
  if (.not. ieee_support_rounding(requested, x64)) error stop 4

  requested = ieee_away
  if (ieee_support_rounding(requested)) error stop 5
  requested = ieee_other
  if (ieee_support_rounding(requested, x64)) error stop 6

  print '(a)', 'ok'
end program ar53_ieee_rounding_support
