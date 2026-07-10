! CHECK: i4_empty -2147483648 2147483647
! CHECK: i8_empty -9223372036854775808 9223372036854775807
! CHECK: i4_mask -2147483648 2147483647
! CHECK: i8_mask -9223372036854775808 9223372036854775807
! CHECK: i4_dim -2147483648 -2147483648 2147483647 2147483647
! CHECK: i8_dim -9223372036854775808 -9223372036854775808 9223372036854775807 9223372036854775807
! CHECK: r4_empty T
! CHECK: r8_empty T
! CHECK: r4_mask T
! CHECK: r8_mask T
! CHECK: r4_dim T
! CHECK: r8_dim T
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar3_empty_reduction
  implicit none

  integer :: zi4(0), z2(0,2), i4_max_dim(2), i4_min_dim(2)
  integer :: vi4(3) = [1, 2, 3]
  integer(8) :: zi8(0), z82(0,2), i8_max_dim(2), i8_min_dim(2)
  integer(8) :: vi8(3) = [1_8, 2_8, 3_8]
  real :: zr4(0), zr4d(0,2), r4_max_dim(2), r4_min_dim(2)
  real :: vr4(2) = [1.0, 2.0]
  real(8) :: zr8(0), zr8d(0,2), r8_max_dim(2), r8_min_dim(2)
  real(8) :: vr8(2) = [1.0_8, 2.0_8]
  logical :: none3(3) = [.false., .false., .false.]
  logical :: none2(2) = [.false., .false.]

  i4_max_dim = maxval(z2, dim=1)
  i4_min_dim = minval(z2, dim=1)
  i8_max_dim = maxval(z82, dim=1)
  i8_min_dim = minval(z82, dim=1)
  r4_max_dim = maxval(zr4d, dim=1)
  r4_min_dim = minval(zr4d, dim=1)
  r8_max_dim = maxval(zr8d, dim=1)
  r8_min_dim = minval(zr8d, dim=1)

  print '(a,2(1x,i0))', 'i4_empty', maxval(zi4), minval(zi4)
  print '(a,2(1x,i0))', 'i8_empty', maxval(zi8), minval(zi8)
  print '(a,2(1x,i0))', 'i4_mask', maxval(vi4, mask=none3), minval(vi4, mask=none3)
  print '(a,2(1x,i0))', 'i8_mask', maxval(vi8, mask=none3), minval(vi8, mask=none3)
  print '(a,4(1x,i0))', 'i4_dim', i4_max_dim, i4_min_dim
  print '(a,4(1x,i0))', 'i8_dim', i8_max_dim, i8_min_dim

  print '(a,1x,l1)', 'r4_empty', maxval(zr4) == -huge(0.0) .and. minval(zr4) == huge(0.0)
  print '(a,1x,l1)', 'r8_empty', maxval(zr8) == -huge(0.0_8) .and. minval(zr8) == huge(0.0_8)
  print '(a,1x,l1)', 'r4_mask', maxval(vr4, mask=none2) == -huge(0.0) .and. &
    minval(vr4, mask=none2) == huge(0.0)
  print '(a,1x,l1)', 'r8_mask', maxval(vr8, mask=none2) == -huge(0.0_8) .and. &
    minval(vr8, mask=none2) == huge(0.0_8)
  print '(a,1x,l1)', 'r4_dim', r4_max_dim(1) == -huge(0.0) .and. &
    r4_max_dim(2) == -huge(0.0) .and. r4_min_dim(1) == huge(0.0) .and. &
    r4_min_dim(2) == huge(0.0)
  print '(a,1x,l1)', 'r8_dim', r8_max_dim(1) == -huge(0.0_8) .and. &
    r8_max_dim(2) == -huge(0.0_8) .and. r8_min_dim(1) == huge(0.0_8) .and. &
    r8_min_dim(2) == huge(0.0_8)
end program ar3_empty_reduction
