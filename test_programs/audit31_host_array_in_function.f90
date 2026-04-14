! Host association threaded into a contained FUNCTION (not just
! SUBROUTINE) when the host var is an ARRAY. Tests that the
! closure-passing ABI handles arrays through a function-result
! signature where the function's hidden host-ref params trail
! the normal positional args.
! CHECK: 60
program t
  implicit none
  integer :: data(3)
  data = [10, 20, 30]
  print *, total()
contains
  integer function total()
    total = data(1) + data(2) + data(3)
  end function
end program
