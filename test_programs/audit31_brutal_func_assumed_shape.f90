! XFAIL: assumed-shape array in FUNCTION returns size()=0 — Task #487
! CHECK: 6
! audit31: FUNCTION with assumed-shape array arg: size() returns 0
! Expected: 6
! Actual:   0
! Subroutine with same signature works correctly.
program audit31_func_assumed
  implicit none
  integer :: data(6), i
  do i = 1, 6
    data(i) = i
  end do
  print *, arr_size(data)
contains
  function arr_size(xs) result(r)
    integer, intent(in) :: xs(:)
    integer :: r
    r = size(xs)
  end function
end program
