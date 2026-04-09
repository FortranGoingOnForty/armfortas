! Stress test: EXIT inside nested loop.
! The optimizer MUST NOT interchange or unswitch this — the EXIT
! creates non-rectangular iteration space that depends on loop order.
program stress_exit_in_nest
  implicit none
  integer :: a(10, 10), i, j
  do i = 1, 10
    do j = 1, 10
      if (j > i) exit
      a(i, j) = i * 10 + j
    end do
  end do
  print *, a(1,1)
  print *, a(5,3)
  print *, a(10,10)
end program
! CHECK: 11
! CHECK: 53
! CHECK: 110
