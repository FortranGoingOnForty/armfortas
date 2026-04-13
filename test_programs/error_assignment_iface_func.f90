! ASSIGNMENT(=) interface must contain subroutines, not functions.
! ERROR_EXPECTED: subroutines, not functions
program t
  implicit none
  interface assignment(=)
    function assign_fn(a, b) result(r)
      integer, intent(out) :: a
      integer, intent(in) :: b
      integer :: r
    end function
  end interface
end program
