program ipo_return_prop
  implicit none

  print *, passthrough(11)
  print *, passthrough(13)

contains

  integer function passthrough(x) result(r)
    integer, intent(in) :: x
    integer :: i, acc

    acc = 0
    do i = 1, 16
      acc = acc + i
    end do

    do i = 1, 16
      acc = acc - i
    end do

    r = x
  end function passthrough

end program ipo_return_prop
! CHECK: 11
! CHECK: 13
