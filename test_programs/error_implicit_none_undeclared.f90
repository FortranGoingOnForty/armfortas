! Undeclared variable under IMPLICIT NONE must be rejected.
! ERROR_EXPECTED: not declared
program t
  implicit none
  x = 42
  print *, x
end program
