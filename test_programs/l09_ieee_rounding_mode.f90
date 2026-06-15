! IEEE rounding-mode get/set actually changes hardware rounding, and the
! optimizer does not merge or reorder FP ops across the mode change. The
! operands come from command_argument_count() (0 at runtime) so they are
! opaque to constant folding: this exercises the real fdiv under each
! mode, not a compile-time fold. A dropped barrier would CSE the two
! identical divisions and print F.
!
! CHECK: up-higher T
! CHECK: differ T
! CHECK: restored T
program l09_ieee_rounding_mode
  use ieee_arithmetic
  implicit none
  type(ieee_round_type) :: saved
  real(8) :: a, b, r_up, r_down
  integer :: n

  n = command_argument_count()
  a = 1.0_8 + real(n, 8)
  b = 3.0_8 + real(n, 8)

  call ieee_get_rounding_mode(saved)

  call ieee_set_rounding_mode(ieee_up)
  r_up = a / b
  call ieee_set_rounding_mode(ieee_down)
  r_down = a / b
  call ieee_set_rounding_mode(saved)

  print *, 'up-higher', (r_up > r_down)
  print *, 'differ', (r_up /= r_down)
  print *, 'restored', (ieee_get_rounding_mode_tag() == saved)
contains
  ! Read the mode back after restore through the same get path.
  integer function ieee_get_rounding_mode_tag() result(t)
    type(ieee_round_type) :: m
    call ieee_get_rounding_mode(m)
    t = m
  end function
end program
