! Whole-array ELEMENTAL calls should lower as elementwise concurrent maps.
! CHECK: 9
! CHECK: 21
! CHECK: 24
! IR_CHECK: doconc_check_
! IR_CHECK: call @shift_scale(
program elemental_array_map
  implicit none
  integer :: a(4), b(4), i

  do i = 1, 4
    a(i) = i * 2
  end do

  b = shift_scale(a, 5)
  print *, b(1)
  print *, b(4)

  b = shift_scale(10, a)
  print *, b(2)

contains

  elemental function shift_scale(x, y) result(r)
    integer, intent(in) :: x, y
    integer :: r

    r = x * 2 + y
  end function shift_scale

end program elemental_array_map
