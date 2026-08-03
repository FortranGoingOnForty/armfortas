! IEEE_NEXT_AFTER must raise overflow or underflow together with inexact
! when a one-step transition crosses those exceptional boundaries. Ordinary
! finite steps must not manufacture any of the three flags.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_ieee_next_after_r8(
! IR_CHECK: call @afs_ieee_next_after_r4(
program ar53_ieee_next_after_flags
  use iso_fortran_env, only: real32, real64
  use ieee_arithmetic, only: ieee_is_finite, ieee_next_after, ieee_positive_inf, ieee_value
  use ieee_exceptions, only: ieee_get_flag, ieee_inexact, ieee_overflow, &
                             ieee_set_flag, ieee_underflow
  implicit none

  real(real64) :: x64, y64, z64
  real(real32) :: x32, y32, z32
  logical :: overflowed, underflowed, inexact

  call clear_test_flags()
  x64 = huge(0.0_real64)
  y64 = ieee_value(0.0_real64, ieee_positive_inf)
  z64 = ieee_next_after(x64, y64)
  call read_test_flags(overflowed, underflowed, inexact)
  if (ieee_is_finite(z64)) error stop 1
  if (.not. overflowed) error stop 2
  if (underflowed) error stop 3
  if (.not. inexact) error stop 4

  call clear_test_flags()
  x64 = tiny(0.0_real64)
  y64 = 0.0_real64
  z64 = ieee_next_after(x64, y64)
  call read_test_flags(overflowed, underflowed, inexact)
  if (z64 <= 0.0_real64 .or. z64 >= tiny(0.0_real64)) error stop 5
  if (overflowed) error stop 6
  if (.not. underflowed) error stop 7
  if (.not. inexact) error stop 8

  call clear_test_flags()
  x32 = huge(0.0_real32)
  y32 = ieee_value(0.0_real32, ieee_positive_inf)
  z32 = ieee_next_after(x32, y32)
  call read_test_flags(overflowed, underflowed, inexact)
  if (ieee_is_finite(z32)) error stop 9
  if (.not. overflowed) error stop 10
  if (underflowed) error stop 11
  if (.not. inexact) error stop 12

  call clear_test_flags()
  x32 = 0.0_real32
  y32 = 1.0_real32
  z32 = ieee_next_after(x32, y32)
  call read_test_flags(overflowed, underflowed, inexact)
  if (z32 <= 0.0_real32 .or. z32 >= tiny(0.0_real32)) error stop 13
  if (overflowed) error stop 14
  if (.not. underflowed) error stop 15
  if (.not. inexact) error stop 16

  call clear_test_flags()
  x64 = 1.0_real64
  y64 = 2.0_real64
  z64 = ieee_next_after(x64, y64)
  call read_test_flags(overflowed, underflowed, inexact)
  if (z64 <= x64 .or. z64 >= y64) error stop 17
  if (overflowed) error stop 18
  if (underflowed) error stop 19
  if (inexact) error stop 20

  print '(a)', 'ok'
contains
  subroutine clear_test_flags()
    call ieee_set_flag(ieee_overflow, .false.)
    call ieee_set_flag(ieee_underflow, .false.)
    call ieee_set_flag(ieee_inexact, .false.)
  end subroutine clear_test_flags

  subroutine read_test_flags(has_overflow, has_underflow, has_inexact)
    logical, intent(out) :: has_overflow, has_underflow, has_inexact
    call ieee_get_flag(ieee_overflow, has_overflow)
    call ieee_get_flag(ieee_underflow, has_underflow)
    call ieee_get_flag(ieee_inexact, has_inexact)
  end subroutine read_test_flags
end program ar53_ieee_next_after_flags
