! Partial unrolling: trip count > FULL_UNROLL_MAX=8 prevents full
! unrolling but the partial unroller picks an unroll factor U that
! divides the trip count, clones the body U-1 more times in place,
! and bumps the IV step from 1 to U. The loop is preserved; only
! the body grows.
!
! Trip 16, body inst count is small → U=4 chosen. Loop runs 4 times
! covering 16 iterations (4 unrolled bodies per iteration).
!
! a(i) = i*i for i=1..16; sum should equal 1496.
! CHECK: 1496
! CHECK: 1
! CHECK: 64
! CHECK: 256
program test_loop_partial_unroll
  implicit none
  integer :: i, a(16), s
  do i = 1, 16
    a(i) = i * i
  end do
  s = 0
  do i = 1, 16
    s = s + a(i)
  end do
  print *, s
  print *, a(1)
  print *, a(8)
  print *, a(16)
end program test_loop_partial_unroll
