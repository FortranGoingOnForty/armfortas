! CHECK: ok
! IR_CHECK: call @afs_array_sum_complex4_dim(
! IR_CHECK: call @afs_array_sum_complex4_dim_mask(
! IR_CHECK: call @afs_array_sum_complex8_dim(
! IR_CHECK: call @afs_array_sum_complex8_dim_mask(
! IR_NOT: call @afs_array_sum_int_dim(
! REPRO_CHECK: run

program stdlib_complex_sum_dim
  use, intrinsic :: iso_fortran_env, only: real32, real64
  implicit none

  complex(real32) :: xsp(2,2,2,2), ysp(2,2,2), ymsp(2,2,2)
  complex(real64) :: xdp(2,2,2,2), ydp(2,2,2), ymdp(2,2,2)

  xsp = (1.0_real32, -2.0_real32)
  xdp = (1.0_real64, -2.0_real64)
  ysp = sum(xsp, 4)
  ymsp = sum(xsp, 4, xsp%re > 0.0_real32)
  ydp = sum(xdp, 4)
  ymdp = sum(xdp, 4, xdp%re > 0.0_real64)

  if (.not. all(abs(ysp - (2.0_real32, -4.0_real32)) < 1.0e-5_real32)) error stop 1
  if (.not. all(abs(ymsp - (2.0_real32, -4.0_real32)) < 1.0e-5_real32)) error stop 2
  if (.not. all(abs(ydp - (2.0_real64, -4.0_real64)) < 1.0e-10_real64)) error stop 3
  if (.not. all(abs(ymdp - (2.0_real64, -4.0_real64)) < 1.0e-10_real64)) error stop 4

  print *, 'ok'
end program
