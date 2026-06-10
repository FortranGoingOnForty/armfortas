! Sprint x05 curated program: nested integer DO loops with accumulation.
! CHECK: 1705
program x05_int_loops
  implicit none
  integer :: i, j, acc
  acc = 0
  do i = 1, 10
    do j = 1, i
      acc = acc + i * j
    end do
  end do
  print *, acc
end program x05_int_loops
