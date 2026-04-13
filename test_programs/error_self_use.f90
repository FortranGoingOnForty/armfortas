! A module cannot USE itself.
! ERROR_EXPECTED: cannot USE itself
module m
  use m
  implicit none
  integer :: x = 1
end module
program p
  print *, "should not reach"
end program
