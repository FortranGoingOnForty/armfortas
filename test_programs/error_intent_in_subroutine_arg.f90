! Cannot pass INTENT(IN) variable to INTENT(OUT) dummy.
! This is a separate test for intent-in protection in call contexts.
! ERROR_EXPECTED: cannot assign to intent(in)
program t
  implicit none
  integer :: x = 5
  call wrapper(x)
contains
  subroutine wrapper(a)
    integer, intent(in) :: a
    a = a + 1
  end subroutine
end program
