! Stress test: 3-level nested loop with 3D array.
! Exercises loop tree construction, preheader insertion, and
! interchange legality on deep nesting.
program stress_deep_nest_3d
  implicit none
  integer :: a(5, 5, 5), i, j, k, total
  do i = 1, 5
    do j = 1, 5
      do k = 1, 5
        a(i, j, k) = i * 100 + j * 10 + k
      end do
    end do
  end do
  total = 0
  do i = 1, 5
    do j = 1, 5
      do k = 1, 5
        total = total + a(i, j, k)
      end do
    end do
  end do
  print *, total
  print *, a(1,1,1)
  print *, a(5,5,5)
end program
! CHECK: 41625
! CHECK: 111
! CHECK: 555
