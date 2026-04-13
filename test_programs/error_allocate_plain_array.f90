! ALLOCATE on a fixed-size array (not allocatable or pointer).
! ERROR_EXPECTED: neither
program t
  implicit none
  integer :: arr(10)
  allocate(arr(20))
end program
