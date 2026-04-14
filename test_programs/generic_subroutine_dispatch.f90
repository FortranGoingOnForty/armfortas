! Generic dispatch must work for SUBROUTINEs too, not just functions.
! Previously Stmt::Call ignored NamedInterface callees and emitted
! the generic name directly, producing link errors against _swap.
! CHECK: 2 1
! CHECK: 2.5000000E0     1.5000000E0
module mswap
  implicit none
  interface swap
    module procedure swap_i, swap_r
  end interface
contains
  subroutine swap_i(a, b)
    integer, intent(inout) :: a, b
    integer :: t
    t = a; a = b; b = t
  end subroutine
  subroutine swap_r(a, b)
    real, intent(inout) :: a, b
    real :: t
    t = a; a = b; b = t
  end subroutine
end module
program t
  use mswap
  implicit none
  integer :: i, j
  real :: x, y
  i = 1; j = 2
  x = 1.5; y = 2.5
  call swap(i, j)
  call swap(x, y)
  print *, i, j
  print *, x, y
end program
