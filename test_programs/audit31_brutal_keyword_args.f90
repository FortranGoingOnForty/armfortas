! audit31: keyword arguments are not reordered. Task #482.
! XFAIL: keyword args bound positionally instead of by name
! CHECK: a=          20 b=          10
program audit31_keyword_args
  implicit none
  call sub(b=10, a=20)
contains
  subroutine sub(a, b)
    integer, intent(in) :: a, b
    print *, 'a=', a, 'b=', b
  end subroutine
end program
