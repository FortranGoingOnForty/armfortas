! Stress test: 100x100 array (40KB, near stack threshold).
! Exercises interchange, preheader insertion, and strength
! reduction on a larger working set.
program stress_large_2d
  implicit none
  integer :: a(100, 100), i, j, checksum
  do i = 1, 100
    do j = 1, 100
      a(i, j) = i + j
    end do
  end do
  checksum = 0
  do i = 1, 100
    do j = 1, 100
      checksum = checksum + a(i, j)
    end do
  end do
  print *, checksum
  print *, a(1,1)
  print *, a(100,100)
end program
! CHECK: 1010000
! CHECK: 2
! CHECK: 200
