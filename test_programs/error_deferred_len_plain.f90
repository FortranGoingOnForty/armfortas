! Deferred-length character without allocatable on a dummy arg.
! ERROR_EXPECTED: requires allocatable or pointer
program t
  implicit none
  call bad("hello")
contains
  subroutine bad(s)
    character(len=:) :: s
    print *, s
  end subroutine
end program
