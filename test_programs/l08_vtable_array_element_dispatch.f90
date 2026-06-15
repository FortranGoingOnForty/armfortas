! Dispatch on polymorphic array elements (the compact-tag / shared
! per-descriptor vtable path) and on a scalar (the offset-24 path). A
! polymorphic array's elements share one dynamic type, so one table
! pointer in the descriptor serves every element-wise call in the loop.
!
! CHECK: 30
! CHECK: 10
module l08_arr
  implicit none
  type :: base
  contains
    procedure :: val => val_base
  end type
  type, extends(base) :: leaf
  contains
    procedure :: val => val_leaf
  end type
contains
  integer function val_base(self)
    class(base), intent(in) :: self
    val_base = 1
  end function
  integer function val_leaf(self)
    class(leaf), intent(in) :: self
    val_leaf = 10
  end function
end module

program main
  use l08_arr
  implicit none
  class(base), allocatable :: arr(:)
  class(base), allocatable :: one
  integer :: i, total

  allocate(leaf :: arr(3))
  total = 0
  do i = 1, 3
    total = total + arr(i)%val()
  end do
  print *, total          ! 3 * 10 = 30

  allocate(leaf :: one)
  print *, one%val()       ! scalar dispatch -> 10
end program
