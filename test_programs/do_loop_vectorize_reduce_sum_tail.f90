! Sum reductions with trip counts not divisible by V vectorize the
! head and peel the remaining iterations into the exit block,
! chaining each peeled iteration through the post-loop scalar
! accumulator.
!
! Trip = 31, V = 4 (i32) → head 28 iters + 3 scalar tail.
! Trip = 31, V = 2 (i64) → head 30 iters + 1 scalar tail.
! Sum 1..31 = 31 * 32 / 2 = 496.
program test_do_loop_vectorize_reduce_sum_tail
  implicit none
  integer :: i
  integer, parameter :: n = 31
  integer(4) :: a32(n), s32
  integer(8) :: a64(n), s64

  do i = 1, n
    a32(i) = i
    a64(i) = i
  end do

  s32 = 0
  do i = 1, n
    s32 = s32 + a32(i)
  end do

  s64 = 0_8
  do i = 1, n
    s64 = s64 + a64(i)
  end do

  print *, s32
  print *, s64
end program test_do_loop_vectorize_reduce_sum_tail
