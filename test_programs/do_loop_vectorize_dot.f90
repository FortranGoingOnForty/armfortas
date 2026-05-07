! Manual integer dot product. NeonVectorize Stage 5 dot-product fold:
! detect `s = s + a(i) * b(i)`, vectorize into vload+vload+vmul+vadd
! plus a final vreduce_sum.
!
! sum(i*i for i = 1..32) = 32*33*65/6 = 11440
! CHECK: 11440
program test_do_loop_vectorize_dot
  implicit none
  integer :: i, a(32), b(32), s

  do i = 1, 32
    a(i) = i
    b(i) = i
  end do

  s = 0
  do i = 1, 32
    s = s + a(i) * b(i)
  end do

  print *, s
end program test_do_loop_vectorize_dot
