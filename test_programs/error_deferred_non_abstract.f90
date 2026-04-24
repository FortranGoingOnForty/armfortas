! DEFERRED type-bound procedure requires ABSTRACT type.
! ERROR_EXPECTED: not ABSTRACT
program t
  implicit none
  type :: concrete
  contains
    procedure(iface), deferred :: my_method
  end type
  abstract interface
    subroutine iface(self)
      import :: concrete
      class(concrete), intent(inout) :: self
    end subroutine
  end interface
end program
