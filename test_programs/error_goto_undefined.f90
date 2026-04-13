! GOTO to undefined label is an error.
! ERROR_EXPECTED: not defined
program t
  implicit none
  goto 999
end program
