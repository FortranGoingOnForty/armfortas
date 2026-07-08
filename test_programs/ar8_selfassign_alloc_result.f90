! CHECK: vals= 11 21 31
! CHECK: again= 12 22 32
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar8_selfassign_alloc_result_m
contains
  function bump(raw) result(events)
    integer, intent(in) :: raw(:)
    integer, allocatable :: events(:)
    integer :: i

    allocate(events(size(raw)))
    do i = 1, size(raw)
      events(i) = raw(i) + 1
    end do
  end function bump
end module ar8_selfassign_alloc_result_m

program ar8_selfassign_alloc_result
  use ar8_selfassign_alloc_result_m, only: bump
  implicit none

  integer, allocatable :: x(:)

  x = [10, 20, 30]
  x = bump(x)
  if (size(x) /= 3) error stop 1
  if (x(1) /= 11 .or. x(2) /= 21 .or. x(3) /= 31) error stop 2
  print '(a,3(1x,i0))', 'vals=', x

  x = bump(x)
  if (x(1) /= 12 .or. x(2) /= 22 .or. x(3) /= 32) error stop 3
  print '(a,3(1x,i0))', 'again=', x
  print '(a)', 'ok'
end program ar8_selfassign_alloc_result
