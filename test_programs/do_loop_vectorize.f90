! Ordinary DO full-array maps at O3/Ofast should vectorize through the
! bulk runtime kernels instead of staying as scalar loops.
!
! CHECK: 7
! CHECK: 7
program test_do_loop_vectorize
  implicit none
  integer :: i, a(32), b(32), c(32)

  a = 3
  b = 4

  do i = 1, 32
    c(i) = a(i) + b(i)
  end do

  print *, c(1)
  print *, c(32)
end program test_do_loop_vectorize
