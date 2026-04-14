! XFAIL: pointer-component => with substring RHS rejected — Task #490
! CHECK: ok
! audit31: pointer-component assignment with substring RHS rejected.
! fortsh/common/string_pool.f90 implements an interning pool this way.
! Expected: compiles; ref%data points into pool.
! Actual:   "error: pointer assignment target 'ref' must have pointer
!           attribute" (diagnostic also cites the wrong entity — ref%data
!           is the pointer target, not ref).
module audit31_ptr_substr
  implicit none
  type :: str_ref
    character(len=:), pointer :: data => null()
  end type
  character(len=64), target :: pool(4)
contains
  subroutine intern(ref, slot, length)
    type(str_ref), intent(inout) :: ref
    integer, intent(in) :: slot, length
    ref%data => pool(slot)(1:length)
  end subroutine
end module
