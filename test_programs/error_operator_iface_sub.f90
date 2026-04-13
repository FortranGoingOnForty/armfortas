! Operator interface must contain functions, not subroutines.
! ERROR_EXPECTED: functions, not subroutines
program t
  implicit none
  interface operator(+)
    subroutine add_sub(a, b)
      integer, intent(in) :: a, b
    end subroutine
  end interface
end program
