! Multi-statement DO body — two stores per iteration, both ordinary
! element-wise i32 maps. NeonVectorize should rewrite both into
! VLoad/VAdd/VStore form (or fall back to two scalar bodies if the
! pass can't see through them).
!
! CHECK: 8
! CHECK: 39
! CHECK: 6
! CHECK: 192
program test_do_loop_vectorize_multi
  implicit none
  integer :: i, a(32), b(32), c(32), d(32)
  integer :: scale

  do i = 1, 32
    b(i) = i
  end do

  scale = 7

  do i = 1, 32
    a(i) = b(i) + scale
    c(i) = b(i) * 6
  end do

  d = c

  print *, a(1)
  print *, a(32)
  print *, c(1)
  print *, d(32)
end program test_do_loop_vectorize_multi
