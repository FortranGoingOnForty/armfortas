! Sum reductions over `acc + unary(load)` patterns. The matcher
! lifts the per-element unary into the vector lane and the
! existing VAdd / vreduce_sum chain handles the rest.
!
! - Negated sum:   `s = s + (-a(i))`         (i32, INeg → VNeg)
! - L1-norm style: `s = s + abs(b(i))`       (f32, FAbs → VAbs)
!
! For a(i) = i over i=1..32, sum(-a(i)) = -528.
! For b(i) = i - 16 over i=1..32, sum(|b(i)|) = 240 + 16 = 256.
program test_do_loop_vectorize_reduce_sum_unary
  implicit none
  integer :: i, s_neg, a(32)
  real(4) :: s_abs, b(32)

  do i = 1, 32
    a(i) = i
    b(i) = real(i - 16, 4)
  end do

  s_neg = 0
  do i = 1, 32
    s_neg = s_neg + (-a(i))
  end do

  s_abs = 0.0
  do i = 1, 32
    s_abs = s_abs + abs(b(i))
  end do

  print *, s_neg
  print *, s_abs
end program test_do_loop_vectorize_reduce_sum_unary
