! Deferred-length character requires ALLOCATABLE or POINTER.
! ERROR_EXPECTED: requires allocatable or pointer
program t
  implicit none
  character(len=:) :: s
  s = "hello"
end program
