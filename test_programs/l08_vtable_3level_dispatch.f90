! Three-level EXTENDS chain with an override at each level. Dispatch
! through a CLASS(base) reference resolves to the dynamic type's method
! via the const vtable (descriptor offset 32 -> table -> slot).
!
! CHECK: 1
! CHECK: 2
! CHECK: 3
! IR_CHECK: vtable_dispatch_call
! IR_NOT: tbp_dispatch_test
module l08_levels
  implicit none
  type :: base
  contains
    procedure :: level => level_base
  end type
  type, extends(base) :: mid
  contains
    procedure :: level => level_mid
  end type
  type, extends(mid) :: leaf
  contains
    procedure :: level => level_leaf
  end type
contains
  integer function level_base(self)
    class(base), intent(in) :: self
    level_base = 1
  end function
  integer function level_mid(self)
    class(mid), intent(in) :: self
    level_mid = 2
  end function
  integer function level_leaf(self)
    class(leaf), intent(in) :: self
    level_leaf = 3
  end function

  subroutine report(obj)
    class(base), intent(in) :: obj
    print *, obj%level()
  end subroutine
end module

program main
  use l08_levels
  implicit none
  type(base) :: b
  type(mid) :: m
  type(leaf) :: l
  call report(b)
  call report(m)
  call report(l)
end program
