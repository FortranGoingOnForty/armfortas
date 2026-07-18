! IEEE_SCALB must scale the input directly instead of materializing 2**I,
! which can overflow or underflow while the final result is representable.
! CHECK: real64 T T
! CHECK: real32 T T
program ieee_scalb_extreme_exponents
  use, intrinsic :: ieee_arithmetic, only: ieee_is_finite, ieee_scalb
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none
  real(real64) :: up64, down64
  real(real32) :: up32, down32

  up64 = ieee_scalb(tiny(0.0_real64), 1024)
  down64 = ieee_scalb(huge(0.0_real64), -1075)
  up32 = ieee_scalb(tiny(0.0_real32), 128)
  down32 = ieee_scalb(huge(0.0_real32), -150)

  print *, 'real64', ieee_is_finite(up64) .and. up64 == 4.0_real64, &
                      ieee_is_finite(down64) .and. down64 > 0.0_real64
  print *, 'real32', ieee_is_finite(up32) .and. up32 == 4.0_real32, &
                      ieee_is_finite(down32) .and. down32 > 0.0_real32
end program ieee_scalb_extreme_exponents
