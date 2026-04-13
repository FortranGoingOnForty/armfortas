! DEALLOCATE on a plain scalar.
! ERROR_EXPECTED: neither
program t
  implicit none
  integer :: x = 10
  deallocate(x)
end program
