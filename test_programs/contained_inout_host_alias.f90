! CHECK: ok
! OPT_EQ: O0,O1,O2,O3 => stdout|stderr|exit
program contained_inout_host_alias
  implicit none
  integer :: limit

  limit = 4
  call grow(limit)
  print *, 'ok'

contains

  subroutine grow(value)
    integer, intent(inout) :: value
    integer :: padding

    ! Keep this helper above the O1 inlining threshold.  The optimizer must
    ! preserve the alias between VALUE and the host-associated LIMIT argument.
    padding = value + 1
    padding = padding + 2
    padding = padding + 3
    padding = padding + 4
    padding = padding + 5
    padding = padding + 6
    padding = padding + 7
    padding = padding + 8
    padding = padding + 9
    padding = padding + 10
    padding = padding + 11
    padding = padding + 12
    padding = padding + 13
    padding = padding + 14
    padding = padding + 15
    padding = padding + 16
    if (padding == -1) print *, padding

    value = value * 2
    if (limit /= value) error stop 2
  end subroutine grow

end program contained_inout_host_alias
