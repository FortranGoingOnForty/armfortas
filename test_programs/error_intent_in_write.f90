! Cannot assign to INTENT(IN) dummy argument.
! ERROR_EXPECTED: cannot assign to intent(in)
program t
  implicit none
  integer :: x = 5
  call bad(x)
contains
  subroutine bad(a)
    integer, intent(in) :: a
    a = 10
  end subroutine
end program
