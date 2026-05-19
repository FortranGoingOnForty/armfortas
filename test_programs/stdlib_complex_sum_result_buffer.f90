! CHECK: ok
! IR_CHECK: call @afs_array_sum_complex4(
! IR_CHECK: call @afs_array_sum_complex4_mask(
! REPRO_CHECK: run
program stdlib_complex_sum_result_buffer
  implicit none
  integer :: i
  complex :: x(4), total, masked
  logical :: mask(4)

  x = [(cmplx(real(i), -real(i)), i=1,4)]
  mask = [.true., .false., .true., .false.]

  total = sum(x)
  masked = sum(x, mask=mask)

  if (abs(real(total) - 10.0) > 1.0e-6) error stop 1
  if (abs(aimag(total) + 10.0) > 1.0e-6) error stop 2
  if (abs(real(masked) - 4.0) > 1.0e-6) error stop 3
  if (abs(aimag(masked) + 4.0) > 1.0e-6) error stop 4

  print *, 'ok'
end program
