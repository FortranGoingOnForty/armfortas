! Computed GOTO with undefined label is an error.
! ERROR_EXPECTED: not defined
program t
  implicit none
  integer :: i = 1
  goto (10, 20, 999), i
10 print *, "ten"
20 print *, "twenty"
end program
