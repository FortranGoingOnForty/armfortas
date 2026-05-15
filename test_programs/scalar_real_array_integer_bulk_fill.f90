! CHECK: ok
! IR_CHECK: int_to_float
! IR_CHECK: call @afs_fill_f64(
! REPRO_CHECK: run
program scalar_real_array_integer_bulk_fill
  implicit none
  real(8) :: values(6)
  real(8) :: poison

  poison = 19.25_8
  values = poison
  values = 0

  if (values(1) /= 0.0_8) error stop 1
  if (values(2) /= 0.0_8) error stop 2
  if (values(3) /= 0.0_8) error stop 3
  if (values(4) /= 0.0_8) error stop 4
  if (values(5) /= 0.0_8) error stop 5
  if (values(6) /= 0.0_8) error stop 6
  print *, 'ok'
end program
