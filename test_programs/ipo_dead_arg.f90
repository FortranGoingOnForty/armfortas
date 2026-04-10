program ipo_dead_arg
  implicit none

  print *, helper(3, 99, 4)

contains

  recursive integer function helper(x, unused, n) result(r)
    integer, intent(in) :: x, unused, n

    if (n <= 1) then
      r = x
    else
      r = helper(x + 1, unused, n - 1)
    end if
  end function helper

end program ipo_dead_arg
! CHECK: 6
