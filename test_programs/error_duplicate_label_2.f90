! Duplicate labels across different statement types.
! ERROR_EXPECTED: duplicate label
program t
  implicit none
  integer :: i
100 i = 1
100 print *, i
end program
