! Element-wise MIN/MAX (select-of-compare shape) over arrays carrying
! a NaN lane. The portable contract (x10 pitfall) is PER-TARGET:
! vectorized O3/Ofast output must equal scalar O0 output on the same
! target — pinned by OPT_EQ below. The NaN POLICY itself is
! processor-dependent and genuinely differs: x86 lowers max/min to a
! select-of-compare (NaN compares false, takes the second operand),
! arm64 to fmax/fmin (NaN propagates). So the NaN lane's outcome is
! printed (and held level-consistent by OPT_EQ) but not asserted to a
! cross-target value; the finite lanes are identical under both
! policies and are asserted exactly.
! CHECK: nanleak
! CHECK: finite_maxsum10 470
! CHECK: finite_minsum10 -160
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

  ! Lane 5 is processor-dependent (see header). Count leaks and print
  ! the count — OPT_EQ holds it identical across levels per target.
  nanleak = 0
  do i = 1, 8
    if (ieee_is_nan(cmax(i))) nanleak = nanleak + 1
    if (ieee_is_nan(cmin(i))) nanleak = nanleak + 1
  end do
  print '(A,1X,I0)', 'nanleak', nanleak

  ! The finite lanes agree under both NaN policies:
  ! cmax(/=5) = 8,7,6,5,6,7,8 (sum 47); cmin(/=5) = 1,2,3,4,3,2,1
  ! (sum 16). Integer-scaled prints: the F edit descriptor currently
  ! falls back to E-notation (runtime gap, noted_items/l05).
  maxsum = 0.0
  minsum = 0.0
  do i = 1, 8
    if (i == 5) cycle
    maxsum = maxsum + cmax(i)
    minsum = minsum - cmin(i)
  end do
  print '(A,1X,I0)', 'finite_maxsum10', nint(maxsum * 10.0)
  print '(A,1X,I0)', 'finite_minsum10', nint(minsum * 10.0)
  print *, 'ok'
end program
