! Cannot write to component of INTENT(IN) derived type.
! ERROR_EXPECTED: cannot assign to intent(in)
program t
  implicit none
  type :: point
    real :: x, y
  end type
  type(point) :: p
  p%x = 1.0; p%y = 2.0
  call bad(p)
contains
  subroutine bad(p)
    type(point), intent(in) :: p
    p%x = 99.0
  end subroutine
end program
