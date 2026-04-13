! Cannot assign to named constant (PARAMETER).
! ERROR_EXPECTED: cannot assign to named constant
program t
  implicit none
  integer, parameter :: N = 10
  N = 20
end program
