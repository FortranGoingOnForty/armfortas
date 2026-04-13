! Duplicate statement labels are forbidden.
! ERROR_EXPECTED: duplicate label
program t
  implicit none
10 print *, "first"
10 print *, "second"
end program
