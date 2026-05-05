! Trip counts that aren't a multiple of V should still vectorize.
! NeonVectorize splits the loop into a vector head (head_count =
! (trip / V) * V iterations) and a scalar tail (trip % V scalar
! iterations peeled into the exit block).
!
! Here trip = 31, V = 4, so head_count = 28 and tail_count = 3.
! The result array elements 1..28 are filled by the vector head,
! and 29..31 are filled by the peeled tail.
program test_do_loop_vectorize_scalar_tail
  implicit none
  integer :: i
  integer, parameter :: n = 31
  integer :: a(n), b(n), c(n)
  integer :: total

  do i = 1, n
    a(i) = i
    b(i) = 2 * i
  end do

  do i = 1, n
    c(i) = a(i) + b(i)
  end do

  total = 0
  do i = 1, n
    total = total + c(i)
  end do

  ! sum_{i=1..31} (3 * i) = 3 * 31 * 32 / 2 = 1488
  print *, total
  print *, c(1)
  print *, c(28)
  print *, c(29)
  print *, c(31)
end program test_do_loop_vectorize_scalar_tail
