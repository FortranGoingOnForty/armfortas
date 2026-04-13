! Named constant (PARAMETER) cannot be ALLOCATABLE.
! ERROR_EXPECTED: cannot be allocatable
program t
  implicit none
  integer, allocatable, parameter :: x = 10
end program
