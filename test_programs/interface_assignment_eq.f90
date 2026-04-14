! INTERFACE ASSIGNMENT(=) dispatches `lhs = rhs` through a
! user-defined subroutine when the LHS and RHS types differ
! (here, type(wrap) = integer). Previously the default store
! path memcpy'd an integer value as if it were a struct pointer.
! CHECK: 42
module asgnmod
  implicit none
  type :: wrap
    integer :: v
  end type
  interface assignment(=)
    module procedure assign_wrap
  end interface
contains
  subroutine assign_wrap(lhs, rhs)
    type(wrap), intent(out) :: lhs
    integer, intent(in) :: rhs
    lhs%v = rhs * 2
  end subroutine
end module
program t
  use asgnmod
  implicit none
  type(wrap) :: w
  w = 21
  print *, w%v
end program
