! Multi-control DO CONCURRENT must iterate the full Cartesian product.
! CHECK: 11 12 21 22 31 32
program do_concurrent_multi_control
  implicit none
  integer :: i, j, arr(3, 2)

  do concurrent (i = 1:3, j = 1:2)
    arr(i, j) = i * 10 + j
  end do

  print *, arr(1, 1), arr(1, 2), arr(2, 1), arr(2, 2), arr(3, 1), arr(3, 2)
end program
