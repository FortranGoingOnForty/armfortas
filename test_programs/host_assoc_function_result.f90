! Host association: contained FUNCTION (as opposed to SUBROUTINE)
! reads a host-local and returns a value computed from it. Exercises
! the Function arm's host_ref_infos path.
! CHECK: 50
program host_func
  implicit none
  integer :: base
  base = 10
  print *, scaled(5)
contains
  function scaled(k) result(r)
    integer, intent(in) :: k
    integer :: r
    r = base * k
  end function
end program
