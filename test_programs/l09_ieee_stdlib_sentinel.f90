! The stdlib pattern this sprint unblocks: ieee_value(..., ieee_quiet_nan)
! as a NaN sentinel returned from a function, then detected with
! ieee_is_nan at the call site (stdlib_math / linalg use this shape). The
! sentinel must survive being returned through a function boundary and an
! optimizer that might fold NaN comparisons.
!
! CHECK: sentinel
! CHECK: real    5.0000
module l09_sentinel
  use ieee_arithmetic
  implicit none
contains
  function safe_div(a, b) result(r)
    real(8), intent(in) :: a, b
    real(8) :: r
    if (b == 0.0_8) then
      r = ieee_value(1.0_8, ieee_quiet_nan)
    else
      r = a / b
    end if
  end function
end module

program main
  use l09_sentinel
  use ieee_arithmetic
  implicit none
  real(8) :: x, y
  integer :: n
  n = command_argument_count()
  x = safe_div(1.0_8, real(n, 8))    ! divide by 0 -> NaN sentinel
  if (ieee_is_nan(x)) then
    print *, 'sentinel'
  else
    print *, 'no sentinel', x
  end if
  y = safe_div(1.0_8, 2.0_8 + real(n, 8))
  print *, 'real', y                  ! 0.5
end program
