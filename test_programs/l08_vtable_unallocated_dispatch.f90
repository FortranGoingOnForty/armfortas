! Dispatching a type-bound procedure through an unallocated polymorphic
! variable has a null vtable pointer in the descriptor. The vtable
! dispatch path detects the null table and ERROR STOPs rather than
! jumping through a null pointer.
!
! STDERR_CHECK: ERROR STOP
! EXIT_CODE: 1
module l08_unalloc
  implicit none
  type :: base
  contains
    procedure :: val => val_base
  end type
contains
  integer function val_base(self)
    class(base), intent(in) :: self
    val_base = 7
  end function
end module

program main
  use l08_unalloc
  implicit none
  class(base), allocatable :: s
  integer :: x
  ! s is never allocated: descriptor vtable pointer is null.
  x = s%val()
  print *, x
end program
