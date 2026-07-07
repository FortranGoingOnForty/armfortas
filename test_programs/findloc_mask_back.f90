! Audit C5: FINDLOC ignored MASK= and BACK=. The inline rank-1 scan took the
! first unconditional match, so `back=.true.` still returned the first index
! and `mask=` was never consulted. Now the scan gates each element on the
! mask and, under BACK, keeps scanning so the last match wins.
program findloc_mask_back
  integer :: a(6) = [5, 2, 3, 2, 5, 2]
  logical :: m(6) = [.false., .true., .false., .false., .false., .true.]

  ! forward: first 2 is at index 2
  print '(A,I2)', 'FW', findloc(a, 2, dim=1)
  ! CHECK: FW 2

  ! back: last 2 is at index 6
  print '(A,I2)', 'BK', findloc(a, 2, dim=1, back=.true.)
  ! CHECK: BK 6

  ! mask: eligible 2s are at indices 2 and 6; first is 2
  print '(A,I2)', 'MK', findloc(a, 2, dim=1, mask=m)
  ! CHECK: MK 2

  ! mask + back: last eligible 2 is at index 6
  print '(A,I2)', 'MB', findloc(a, 2, dim=1, mask=m, back=.true.)
  ! CHECK: MB 6

  ! no match returns 0
  print '(A,I2)', 'NF', findloc(a, 9, dim=1)
  ! CHECK: NF 0

  ! scalar .false. mask excludes everything
  print '(A,I2)', 'SF', findloc(a, 5, dim=1, mask=.false.)
  ! CHECK: SF 0
end program
