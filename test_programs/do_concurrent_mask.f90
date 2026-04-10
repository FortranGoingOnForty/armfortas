! Masked DO CONCURRENT must skip filtered-out iterations.
! CHECK: 0 2 0 4 0 6
! IR_CHECK: doconc_check_
! IR_CHECK: if_then_
program do_concurrent_mask
  implicit none
  integer :: i, arr(6)

  do i = 1, 6
    arr(i) = 0
  end do
  do concurrent (i = 1:6, mod(i, 2) == 0)
    arr(i) = i
  end do

  print *, arr
end program
