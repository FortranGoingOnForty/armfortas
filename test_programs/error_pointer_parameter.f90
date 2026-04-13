! Named constant (PARAMETER) cannot be a POINTER.
! ERROR_EXPECTED: cannot be a pointer
program t
  implicit none
  integer, pointer, parameter :: x = 10
end program
