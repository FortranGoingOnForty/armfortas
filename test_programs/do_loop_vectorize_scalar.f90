! Ordinary DO full-array map with a loop-invariant scalar addend should
! vectorize through real NEON ops at O3 (preferred) or fall back to the
! bulk afs_array_add_scalar runtime kernel.
!
! CHECK: 8
! CHECK: 39
program test_do_loop_vectorize_scalar
  implicit none
  integer :: i, a(32), b(32)
  integer :: scale

  do i = 1, 32
    b(i) = i
  end do

  scale = 7

  do i = 1, 32
    a(i) = b(i) + scale
  end do

  print *, a(1)
  print *, a(32)
end program test_do_loop_vectorize_scalar
