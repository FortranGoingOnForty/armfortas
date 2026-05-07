! Reduction-loop partial unrolling. Trip 16 → U=4 (largest factor
! of 16 fitting the body budget). The latch carries the accumulator
! through the U-way unrolled body so the result is identical to the
! unrolled scalar version.
!
! sum(i, i=1..16) = 136.
! sum(i*i, i=1..16) = 1496.
!
! CHECK: 136
! CHECK: 1496
program test_loop_partial_unroll_reduction
  implicit none
  integer :: i, a(16), b(16), s, t

  do i = 1, 16
    a(i) = i
    b(i) = i * i
  end do

  s = 0
  do i = 1, 16
    s = s + a(i)
  end do
  print *, s

  t = 0
  do i = 1, 16
    t = t + b(i)
  end do
  print *, t
end program test_loop_partial_unroll_reduction
