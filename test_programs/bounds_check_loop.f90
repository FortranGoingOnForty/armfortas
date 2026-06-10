program bounds_check_loop
  implicit none
  integer :: i, a(4), s

  a = [1, 2, 3, 4]
  s = 0
  do i = 1, 4
    s = s + a(i)
  end do

  print *, s
end program bounds_check_loop
! CHECK: 10
! IR_NOT: rt_call @__afs_check_bounds
