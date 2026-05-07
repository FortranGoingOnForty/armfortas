! Pure DO-loop array copy `c(i) = b(i)` with no arithmetic. NeonVectorize
! should rewrite this to a single VLoad + VStore pair, and the older
! Vectorize pass would otherwise dispatch to afs_array_copy_*.
!
! CHECK: 1
! CHECK: 32
program test_do_loop_vectorize_copy
  implicit none
  integer :: i, b(32), c(32)

  do i = 1, 32
    b(i) = i
  end do

  do i = 1, 32
    c(i) = b(i)
  end do

  print *, c(1)
  print *, c(32)
end program test_do_loop_vectorize_copy
