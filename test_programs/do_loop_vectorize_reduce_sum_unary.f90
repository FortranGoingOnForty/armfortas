! Sum reductions over `acc + unary(load)` patterns. The matcher
! lifts the per-element unary into the vector lane and the
! existing VAdd / vreduce_sum chain handles the rest.
!
! - L1-norm style: `s = s + abs(a(i))`
! - Negated sum:   `s = s + (-a(i))`
!
! Composes with scalar tail (trip not divisible by V).
program test_do_loop_vectorize_reduce_sum_unary
  implicit none
  integer :: i, s_neg, a(32)
  real(4) :: s_abs32, b32(32), s_abs32_tail, b32_tail(31)
  real(8) :: s_abs64, b64(32)

  do i = 1, 32
    a(i) = i
    b32(i) = real(i - 16, 4)
    b64(i) = real(i - 16, 8)
  end do
  do i = 1, 31
    b32_tail(i) = real(i - 16, 4)
  end do

  s_neg = 0
  do i = 1, 32
    s_neg = s_neg + (-a(i))
  end do

  s_abs32 = 0.0
  do i = 1, 32
    s_abs32 = s_abs32 + abs(b32(i))
  end do

  s_abs64 = 0.0_8
  do i = 1, 32
    s_abs64 = s_abs64 + abs(b64(i))
  end do

  s_abs32_tail = 0.0
  do i = 1, 31
    s_abs32_tail = s_abs32_tail + abs(b32_tail(i))
  end do

  print *, s_neg
  print *, s_abs32
  print *, s_abs64
  print *, s_abs32_tail
end program test_do_loop_vectorize_reduce_sum_unary
