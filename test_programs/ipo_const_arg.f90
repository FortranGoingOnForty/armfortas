program ipo_const_arg
  implicit none

  print *, compute(3, 4)
  print *, compute(5, 4)

contains

  integer function compute(base, step) result(r)
    integer, intent(in) :: base, step
    integer :: i, acc

    acc = base
    do i = 1, 8
      acc = acc + step
    end do

    do i = 1, 8
      acc = acc - 1
    end do

    do i = 1, 8
      acc = acc + step - 1
    end do

    r = acc + step
  end function compute

end program ipo_const_arg
! CHECK: 55
! CHECK: 57
