! audit31 harvest: defined assignment(=) that converts an integer RHS
! into a derived-type LHS, routed through a module's INTERFACE
! ASSIGNMENT(=) block.  Sprint 31 #466 fixed the segfault; this is
! the canonical runtime check that the RHS is actually passed by
! reference, the conversion procedure runs, and both LHS components
! get populated.
! CHECK: 10 20
module audit31_assn_mod
  implicit none
  type :: pair
    integer :: a = 0, b = 0
  end type
  interface assignment(=)
    module procedure assign_pair_from_int
  end interface
contains
  subroutine assign_pair_from_int(lhs, rhs)
    type(pair), intent(out) :: lhs
    integer, intent(in) :: rhs
    lhs%a = rhs
    lhs%b = rhs * 2
  end subroutine
end module

program audit31_assn_cross_module
  use audit31_assn_mod
  implicit none
  type(pair) :: q
  q = 10
  print *, q%a, q%b
end program
