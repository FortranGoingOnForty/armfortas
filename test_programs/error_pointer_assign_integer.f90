! Pointer assignment (=>) to non-pointer variable.
! ERROR_EXPECTED: must have pointer attribute
program t
  implicit none
  integer :: x = 10
  integer, target :: y = 20
  x => y
end program
