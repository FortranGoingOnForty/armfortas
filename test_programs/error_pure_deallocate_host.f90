! PURE cannot DEALLOCATE a host-associated variable (F2018 15.7).
! XFAIL: XFAIL-002 DEALLOCATE in PURE not yet checked against host association
! ERROR_EXPECTED: host or use association
program t
  implicit none
  integer, allocatable :: buf(:)
contains
  pure subroutine bad()
    deallocate(buf)
  end subroutine
end program
