! CHECK: ok
! IR_CHECK: global @afs_mod_complex_param_data_c_sp: [[f32 x 2] x 4] = [-10, 0, 2, 0, 3, 0, 4, 0]
! IR_CHECK: global @afs_mod_complex_param_data_d_sp: [[f32 x 2] x 6] = [-10, 0, 2, 0, 3, 0, 4, 0, 7, -1, 7, -1]
! REPRO_CHECK: run

module complex_param_data
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none

  real(real32), parameter :: r_sp(4) = [-10.0_real32, 2.0_real32, 3.0_real32, 4.0_real32]
  real(real64), parameter :: r_dp(4) = [-10.0_real64, 2.0_real64, 3.0_real64, 4.0_real64]
  complex(real32), parameter :: c_sp(4) = r_sp
  complex(real64), parameter :: c_dp(4) = r_dp
  complex(real32) :: d_sp(3,2) = reshape(c_sp, [3,2], [(7.0_real32, -1.0_real32)])
end module

program stdlib_complex_param_array_from_real
  use, intrinsic :: iso_fortran_env, only: real32, real64
  use complex_param_data, only: c_sp, c_dp, d_sp
  implicit none

  if (.not. (abs(real(c_sp(1)) + 10.0_real32) < 1.0e-5_real32)) error stop 1
  if (.not. (abs(aimag(c_sp(1))) < 1.0e-5_real32)) error stop 2
  if (.not. (abs(real(c_dp(2)) - 2.0_real64) < 1.0e-10_real64)) error stop 3
  if (.not. (abs(aimag(c_dp(2))) < 1.0e-10_real64)) error stop 4
  if (.not. (abs(real(d_sp(2,2)) - 7.0_real32) < 1.0e-5_real32)) error stop 5
  if (.not. (abs(aimag(d_sp(2,2)) + 1.0_real32) < 1.0e-5_real32)) error stop 6

  print *, 'ok'
end program
