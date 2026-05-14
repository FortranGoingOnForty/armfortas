! CHECK: ok
! IR_CHECK: call @afs_array_sum_real8_mask(
! IR_CHECK: call @afs_array_sum_int_mask(
! REPRO_CHECK: run
program stdlib_sum_positional_mask
  implicit none
  real :: a(5)
  integer :: ai(5)
  logical :: m(5)

  a = [1.0, 2.0, 3.0, 4.0, 5.0]
  ai = [1, 2, 3, 4, 5]
  m = [.true., .false., .true., .false., .true.]

  if (abs(sum(a, m) - 9.0) > 1.0e-6) error stop 1
  if (sum(ai, m) /= 9) error stop 2
  if (abs(sum(a, .not. m) - 6.0) > 1.0e-6) error stop 3

  print *, 'ok'
end program
