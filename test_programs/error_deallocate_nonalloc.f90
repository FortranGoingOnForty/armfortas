! DEALLOCATE requires ALLOCATABLE or POINTER variable.
! ERROR_EXPECTED: neither
program t
  implicit none
  integer :: x
  deallocate(x)
end program
