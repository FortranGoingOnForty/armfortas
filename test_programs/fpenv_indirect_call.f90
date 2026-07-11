! CHECK: 3FF0000000000000 3FF0000000000001
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program fpenv_indirect_call
  use, intrinsic :: ieee_arithmetic
  use iso_fortran_env, only: real64, int64
  implicit none
  abstract interface
    subroutine action()
    end subroutine action
  end interface
  procedure(action), pointer :: p
  integer :: n
  real(real64) :: a, b, rounded_down, rounded_up

  n = command_argument_count()
  a = real(n + 1, real64)
  b = 5.5511151231257827e-17_real64

  p => set_down
  call p()
  rounded_down = a + b
  p => set_up
  call p()
  rounded_up = a + b
  p => set_nearest
  call p()

  print '(z16.16,1x,z16.16)', transfer(rounded_down, 0_int64), &
    transfer(rounded_up, 0_int64)
contains
  subroutine set_down()
    call ieee_set_rounding_mode(ieee_down)
  end subroutine set_down

  subroutine set_up()
    call ieee_set_rounding_mode(ieee_up)
  end subroutine set_up

  subroutine set_nearest()
    call ieee_set_rounding_mode(ieee_nearest)
  end subroutine set_nearest
end program fpenv_indirect_call
