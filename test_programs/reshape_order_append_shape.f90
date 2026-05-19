! CHECK: ok
! IR_CHECK: call @afs_array_reshape
! REPRO_CHECK: run
program reshape_order_append_shape
  implicit none

  real(8) :: d(3, 2)
  real(8), allocatable :: d3(:, :)

  d = reshape([1.0d0, 2.0d0, 3.0d0, 4.0d0, 5.0d0, 6.0d0], [3, 2])
  d3 = reshape([transpose(d), transpose(d)], [6, 2], order=[2, 1])

  if (any(shape(d3) /= [6, 2])) error stop 1
  if (any(d3(:, 1) /= [1.0d0, 2.0d0, 3.0d0, 1.0d0, 2.0d0, 3.0d0])) error stop 2
  if (any(d3(:, 2) /= [4.0d0, 5.0d0, 6.0d0, 4.0d0, 5.0d0, 6.0d0])) error stop 3

  print *, 'ok'
end program reshape_order_append_shape
