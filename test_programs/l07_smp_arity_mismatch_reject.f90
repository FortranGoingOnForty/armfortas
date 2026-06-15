! l07: a separate module procedure body with a different number of dummy
! arguments than its interface is rejected (F2008 C1418).
! FLAGS: --std=f2023
! ERROR_EXPECTED: dummy argument(s) but its interface declares
module l07am
  implicit none
  interface
    module subroutine s(x)
      integer, intent(in) :: x
    end subroutine
  end interface
end module
submodule (l07am) impl
contains
  module subroutine s(x, y)
    integer, intent(in) :: x, y
  end subroutine
end submodule
