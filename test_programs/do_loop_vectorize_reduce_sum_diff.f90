! Sum-of-differences reduction: `s = s + (a(i) - b(i))`. The body
! has two array loads feeding a sub feeding the accumulator add.
! Lifts both loads to VLoad, the sub to VSub, the add to VAdd, and
! finishes with vreduce_sum.
program test_do_loop_vectorize_reduce_sum_diff
  implicit none
  integer :: i, s_int
  real(4) :: s_f32, a32(32), b32(32)
  real(8) :: s_f64, a64(32), b64(32)
  integer :: ai(32), bi(32)

  do i = 1, 32
    ai(i) = i * 2
    bi(i) = i
    a32(i) = real(i * 2, 4)
    b32(i) = real(i, 4)
    a64(i) = real(i * 2, 8)
    b64(i) = real(i, 8)
  end do

  s_int = 0
  do i = 1, 32
    s_int = s_int + (ai(i) - bi(i))
  end do

  s_f32 = 0.0
  do i = 1, 32
    s_f32 = s_f32 + (a32(i) - b32(i))
  end do

  s_f64 = 0.0_8
  do i = 1, 32
    s_f64 = s_f64 + (a64(i) - b64(i))
  end do

  ! All three should equal sum(i, i=1..32) = 528.
  print *, s_int
  print *, s_f32
  print *, s_f64
end program test_do_loop_vectorize_reduce_sum_diff
