! l07: a separate module procedure body that redeclares a dummy with a
! type differing from its interface is rejected (F2008 C1418). Here the
! interface says integer x; the body says real x.
! FLAGS: --std=f2023
! ERROR_EXPECTED: does not match its interface in the ancestor module
module l07tm
  implicit none
  interface
    module function f(x) result(r)
      integer, intent(in) :: x
      integer :: r
    end function
  end interface
end module
submodule (l07tm) impl
contains
  module function f(x) result(r)
    real, intent(in) :: x
    integer :: r
    r = 1
  end function
end submodule
