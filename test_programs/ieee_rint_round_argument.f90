! IEEE_RINT's optional ROUND argument overrides the ambient mode for one
! operation without changing it; omitting ROUND uses the ambient mode.
! FLAGS: --std=f2023
! CHECK: real64-directed T T T T
! CHECK: real64-nearest-away T T T T
! CHECK: real64-current T T
! CHECK: real32-directed T T T T
! CHECK: real32-nearest-away T T T T
! CHECK: real32-current T T
program ieee_rint_round_argument
  use, intrinsic :: ieee_arithmetic
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none
  type(ieee_round_type) :: saved, current

  call ieee_get_rounding_mode(saved)

  call ieee_set_rounding_mode(ieee_down)
  print *, 'real64-directed', &
       ieee_rint(1.1_real64, ieee_up) == 2.0_real64, &
       ieee_rint(-1.1_real64, ieee_down) == -2.0_real64, &
       ieee_rint(-1.9_real64, ieee_to_zero) == -1.0_real64, &
       ieee_class(ieee_rint(-0.1_real64, ieee_to_zero)) == ieee_negative_zero
  print *, 'real64-nearest-away', &
       ieee_rint(2.5_real64, ieee_nearest) == 2.0_real64, &
       ieee_rint(3.5_real64, ieee_nearest) == 4.0_real64, &
       ieee_rint(2.5_real64, ieee_away) == 3.0_real64, &
       ieee_rint(-2.5_real64, ieee_away) == -3.0_real64
  call ieee_get_rounding_mode(current)
  print *, 'real64-current', current == ieee_down, &
       ieee_rint(1.9_real64) == 1.0_real64

  call ieee_set_rounding_mode(ieee_up)
  print *, 'real32-directed', &
       ieee_rint(1.1_real32, ieee_up) == 2.0_real32, &
       ieee_rint(-1.1_real32, ieee_down) == -2.0_real32, &
       ieee_rint(-1.9_real32, ieee_to_zero) == -1.0_real32, &
       ieee_class(ieee_rint(-0.1_real32, ieee_to_zero)) == ieee_negative_zero
  print *, 'real32-nearest-away', &
       ieee_rint(2.5_real32, ieee_nearest) == 2.0_real32, &
       ieee_rint(3.5_real32, ieee_nearest) == 4.0_real32, &
       ieee_rint(2.5_real32, ieee_away) == 3.0_real32, &
       ieee_rint(-2.5_real32, ieee_away) == -3.0_real32
  call ieee_get_rounding_mode(current)
  print *, 'real32-current', current == ieee_up, &
       ieee_rint(-1.1_real32) == -1.0_real32

  call ieee_set_rounding_mode(saved)
end program ieee_rint_round_argument
