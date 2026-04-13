! Variable cannot be both ALLOCATABLE and POINTER.
! ERROR_EXPECTED: cannot be both allocatable and pointer
program t
  implicit none
  integer, allocatable, pointer :: x
end program
