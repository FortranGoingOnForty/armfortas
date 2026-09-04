! CHECK: ok
! IR_CHECK: direct_maxval_dim_check
! IR_CHECK: direct_minval_dim_check
! IR_CHECK: call @afs_array_maxval_real8_dim
! IR_NOT: call @afs_array_minval_real8_dim
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program maxval_minval_abs_dim_reduction
  implicit none

  real(4) :: r4(2, 2), r4_max(2), r4_min(2)
  real(8) :: r8(2, 3), r8_max(3), r8_min(3)
  real(4) :: c4_max(2)
  complex(4) :: c4(2, 2)
  integer :: i4(2, 2), i4_max(2), i4_min(2)

  r4 = reshape([-1.0_4, 0.0_4, 3.0_4, -4.0_4], [2, 2])
  r4_max = maxval(abs(r4), dim=1)
  r4_min = minval(abs(r4), dim=1)
  if (any(abs(r4_max - [1.0_4, 4.0_4]) > 1.0e-6_4)) error stop 1
  if (any(abs(r4_min - [0.0_4, 3.0_4]) > 1.0e-6_4)) error stop 2

  r8 = reshape([-1.0_8, 2.0_8, 3.0_8, -4.0_8, 5.0_8, -6.0_8], [2, 3])
  r8_max = maxval(abs(r8), dim=1)
  r8_min = minval(abs(r8), dim=1)
  if (any(abs(r8_max - [2.0_8, 4.0_8, 6.0_8]) > 1.0e-10_8)) error stop 3
  if (any(abs(r8_min - [1.0_8, 3.0_8, 5.0_8]) > 1.0e-10_8)) error stop 4

  c4(1, 1) = cmplx(3.0_4, 4.0_4, kind=4)
  c4(2, 1) = cmplx(1.0_4, 0.0_4, kind=4)
  c4(1, 2) = cmplx(0.0_4, 2.0_4, kind=4)
  c4(2, 2) = cmplx(5.0_4, 12.0_4, kind=4)
  c4_max = maxval(abs(c4), dim=1)
  if (any(abs(c4_max - [5.0_4, 13.0_4]) > 1.0e-5_4)) error stop 5

  i4 = reshape([-1, 9, -3, 4], [2, 2])
  i4_max = maxval(i4, dim=1)
  i4_min = minval(i4, dim=1)
  if (any(i4_max /= [9, 4])) error stop 6
  if (any(i4_min /= [-1, -3])) error stop 7

  print *, 'ok'
end program maxval_minval_abs_dim_reduction
