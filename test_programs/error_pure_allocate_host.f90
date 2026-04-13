! PURE cannot ALLOCATE a host-associated variable (F2018 15.7).
! XFAIL: ALLOCATE in PURE not yet checked against host association
! ERROR_EXPECTED: host or use association
program t
  implicit none
  integer, allocatable :: buf(:)
contains
  pure subroutine bad()
    allocate(buf(10))
  end subroutine
end program
