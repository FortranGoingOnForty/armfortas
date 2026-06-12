! Element-wise MIN/MAX (select-of-compare shape) over arrays carrying
! NaN lanes. The contract is per-target: vectorized O3/Ofast output
! must equal the scalar O0 output on the SAME target (minps/maxps
! return the second operand on NaN; the isel operand order reproduces
! the scalar select exactly — x10 pitfall). Assertions avoid printing
! NaN textually; positions and finite values carry the comparison.
! CHECK: nanleak 0
! CHECK: lane5 40 40
! CHECK: maxsum10 510
! CHECK: minsum10 -200
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10_minmax_nan_lanes
  use, intrinsic :: ieee_arithmetic, only: ieee_is_nan
  implicit none
  real :: a(8), b(8), cmax(8), cmin(8)
  real :: nan_val, maxsum, minsum
  integer :: i, nanleak

  nan_val = 0.0
  nan_val = nan_val / nan_val   ! quiet NaN at runtime

  do i = 1, 8
    a(i) = real(i)
    b(i) = real(9 - i)
  end do
  a(5) = nan_val

  do i = 1, 8
    cmax(i) = max(a(i), b(i))
  end do
  do i = 1, 8
    cmin(i) = min(a(i), b(i))
  end do

  ! a(5) is NaN: the select-of-compare lowering takes the second
  ! operand (b(5) = 4.0) on a false compare, so no NaN may leak into
  ! either result — and O3's packed min/max must agree with O0.
  nanleak = 0
  do i = 1, 8
    if (ieee_is_nan(cmax(i))) nanleak = nanleak + 1
    if (ieee_is_nan(cmin(i))) nanleak = nanleak + 1
  end do
  print '(A,1X,I0)', 'nanleak', nanleak
  if (nanleak /= 0) error stop 1
  if (cmax(5) /= 4.0 .or. cmin(5) /= 4.0) error stop 2
  ! Integer-scaled prints: the F edit descriptor currently falls back
  ! to E-notation (runtime gap, noted_items/l05), so assert on nint.
  print '(A,1X,I0,1X,I0)', 'lane5', nint(cmax(5) * 10.0), nint(cmin(5) * 10.0)

  ! cmax = 8,7,6,5,4,6,7,8 ; cmin = 1,2,3,4,4,3,2,1
  maxsum = 0.0
  minsum = 0.0
  do i = 1, 8
    maxsum = maxsum + cmax(i)
    minsum = minsum - cmin(i)
  end do
  print '(A,1X,I0)', 'maxsum10', nint(maxsum * 10.0)
  print '(A,1X,I0)', 'minsum10', nint(minsum * 10.0)
  print *, 'ok'
end program
