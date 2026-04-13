! ASSIGNMENT(=) subroutine must have exactly 2 arguments.
! ERROR_EXPECTED: 2 arguments
program t
  implicit none
  interface assignment(=)
    subroutine assign_sub(a)
      integer, intent(out) :: a
    end subroutine
  end interface
end program
