! Manual sum-reduction loop. NeonVectorize Stage 5: rewrite the
! loop-carried scalar accumulator into a vector accumulator with a
! `vbroadcast(0)` initialiser, vectorize the body's iadd, and emit
! `vreduce_sum` after the loop to collapse the vector back to a
! scalar.
!
! sum(1..32) = 32*33/2 = 528
! CHECK: 528
program test_do_loop_vectorize_reduce_sum
  implicit none
  integer :: i, a(32), s

  do i = 1, 32
    a(i) = i
  end do

  s = 0
  do i = 1, 32
    s = s + a(i)
  end do

  print *, s
end program test_do_loop_vectorize_reduce_sum
