! Test loop fusion: merge two adjacent same-range loops.
! Loop 1 writes to a, loop 2 reads a and writes b.
! After fusion: single loop does both, improving locality.
program loop_fusion
  implicit none
  integer :: a(10), b(10), i
  do i = 1, 10
    a(i) = i * 2
  end do
  do i = 1, 10
    b(i) = a(i) + 1
  end do
  print *, b(1), b(10)
end program
! CHECK: 3 21
