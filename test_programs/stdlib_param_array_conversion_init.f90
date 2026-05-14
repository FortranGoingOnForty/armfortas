! CHECK: ok
! IR_CHECK: global @afs_mod_stdlib_param_array_conversion_data_x1: [f32 x 5] = [1, 2, 3, 4, 5]
! IR_CHECK: global @afs_mod_stdlib_param_array_conversion_data_i1: [i32 x 5] = [1, 2, 3, 4, 5]
! REPRO_CHECK: run

module stdlib_param_array_conversion_data
  use, intrinsic :: iso_fortran_env, only: real32, real64, int32
  implicit none

  real(real64), parameter :: d1(5) = [1.0_real64, 2.0_real64, 3.0_real64, 4.0_real64, 5.0_real64]
  real(real32) :: x1(5) = real(d1, real32)
  integer(int32) :: i1(5) = int(d1, int32)
end module

program stdlib_param_array_conversion_init
  use, intrinsic :: iso_fortran_env, only: real32, int32
  use stdlib_param_array_conversion_data, only: x1, i1
  implicit none

  if (x1(1) /= 1.0_real32) error stop 1
  if (x1(5) /= 5.0_real32) error stop 2
  if (i1(1) /= 1_int32) error stop 3
  if (i1(5) /= 5_int32) error stop 4

  print *, 'ok'
end program
