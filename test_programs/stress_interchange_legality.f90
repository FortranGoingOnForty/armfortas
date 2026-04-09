! Stress test: interchange legality with swapped subscripts.
! a(i,j) = b(i,j) + b(j,i) reads with transposed indices.
! The optimizer MUST NOT interchange this loop — doing so would
! change which b() values are read before being overwritten.
program stress_interchange_legality
  implicit none
  integer :: a(10, 10), b(10, 10), i, j
  do i = 1, 10
    do j = 1, 10
      b(i, j) = i + j
    end do
  end do
  do i = 1, 10
    do j = 1, 10
      a(i, j) = b(i, j) + b(j, i)
    end do
  end do
  print *, a(1,1)
  print *, a(5,5)
  print *, a(3,7)
  print *, a(7,3)
end program
! CHECK: 4
! CHECK: 20
! CHECK: 20
! CHECK: 20
