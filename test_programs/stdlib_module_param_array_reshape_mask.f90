! CHECK: ok
! IR_CHECK: global @afs_mod_param_array_data_d2: [i8 x 6] = [-1, 2, 3, -4, 5, -6]
! IR_CHECK: call @afs_array_sum_real8_mask
! IR_NOT: call @afs_array_sum_real8_dim
! REPRO_CHECK: run
module param_array_data
  use, intrinsic :: iso_fortran_env, only: int8
  implicit none

  integer(int8), parameter :: d1(6) = [-1_int8, 2_int8, 3_int8, -4_int8, 5_int8, -6_int8]
  integer(int8) :: d2(2,3) = reshape(d1, [2,3])
end module

program stdlib_module_param_array_reshape_mask
  use, intrinsic :: iso_fortran_env, only: int8, real64
  use param_array_data, only: d2
  implicit none

  if (d2(1,1) /= -1_int8) error stop 1
  if (d2(2,1) /= 2_int8) error stop 2
  if (d2(1,2) /= 3_int8) error stop 3
  if (d2(2,2) /= -4_int8) error stop 4
  if (d2(1,3) /= 5_int8) error stop 5
  if (d2(2,3) /= -6_int8) error stop 6
  if (count(d2 > 0_int8) /= 3) error stop 7
  if (sum(d2, mask=d2 > 0_int8) /= 10_int8) error stop 8
  if (abs(sum(real(d2, real64), d2 > 0_int8) - 10.0_real64) > 1.0e-10_real64) error stop 9

  print *, 'ok'
end program
