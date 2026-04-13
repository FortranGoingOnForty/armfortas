! PURE cannot write to USE-associated module variable (F2018 15.7).
! ERROR_EXPECTED: host or use association
module m
  implicit none
  integer :: shared = 0
end module
program t
  use m
  implicit none
contains
  pure subroutine bad()
    shared = 42
  end subroutine
end program
