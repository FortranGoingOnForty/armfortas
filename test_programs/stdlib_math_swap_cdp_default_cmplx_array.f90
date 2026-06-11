! CHECK: ok
! REPRO_CHECK: run
! XFAIL(x86_64): X64-O0-003 (no x86 register class for by-value array/complex aggregates)
module m
  use iso_fortran_env, only: real32, real64
  implicit none

  interface swap
    module procedure swap_csp
    module procedure swap_cdp
  end interface
contains
  elemental subroutine swap_csp(lhs, rhs)
    complex(real32), intent(inout) :: lhs, rhs
    complex(real32) :: temp

    temp = lhs
    lhs = rhs
    rhs = temp
  end subroutine

  elemental subroutine swap_cdp(lhs, rhs)
    complex(real64), intent(inout) :: lhs, rhs
    complex(real64) :: temp

    temp = lhs
    lhs = rhs
    rhs = temp
  end subroutine
end module

program p
  use iso_fortran_env, only: real64
  use m, only: swap
  implicit none

  complex(real64) :: x(3), y(3)

  x = cmplx([1, 2, 3], [4, 5, 6])
  y = cmplx([4, 5, 6], [1, 2, 3])

  call swap(x, y)

  if (.not. all(x == cmplx([4, 5, 6], [1, 2, 3]))) error stop 1
  if (.not. all(y == cmplx([1, 2, 3], [4, 5, 6]))) error stop 2

  call swap(x, x)

  if (.not. all(x == cmplx([4, 5, 6], [1, 2, 3]))) error stop 3
  print *, "ok"
end program
