! CHECK: max2 2 2
! CHECK: min2 1 3
! CHECK: max3 1 3 2
! CHECK: min3 2 1 1
! CHECK: back2 1 2
! CHECK: mask3 1 3 2
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar3_maxloc_rank2
  implicit none

  integer :: m(2,3), t(2,3,2), tie(2,2)
  integer :: max2(2), min2(2), back2(2), mask3(3)
  integer :: max3(3), min3(3)
  logical :: keep(2,3,2)

  m = 0
  m(2,2) = 99
  m(1,3) = -7

  t = 0
  t(1,3,2) = 44
  t(2,1,1) = -11

  tie = 1
  tie(1,1) = 7
  tie(1,2) = 7

  keep = .false.
  keep(2,2,1) = .true.
  keep(1,3,2) = .true.

  max2 = maxloc(m)
  min2 = minloc(m)
  max3 = maxloc(t)
  min3 = minloc(t)
  back2 = maxloc(tie, back=.true.)
  mask3 = maxloc(t, mask=keep)

  print '(a,2(1x,i0))', 'max2', max2
  print '(a,2(1x,i0))', 'min2', min2
  print '(a,3(1x,i0))', 'max3', max3
  print '(a,3(1x,i0))', 'min3', min3
  print '(a,2(1x,i0))', 'back2', back2
  print '(a,3(1x,i0))', 'mask3', mask3
end program ar3_maxloc_rank2
