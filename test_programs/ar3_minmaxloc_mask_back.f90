! CHECK: min_fw 2
! CHECK: min_back 6
! CHECK: min_mask 2
! CHECK: min_mask_back 6
! CHECK: max_fw 3
! CHECK: max_back 5
! CHECK: max_mask 5
! CHECK: max_false 0
! CHECK: max_stride_back 3
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar3_minmaxloc_mask_back
  implicit none

  integer :: a(6) = [5, 2, 9, 2, 9, 2]
  logical :: m(6) = [.false., .true., .false., .false., .true., .true.]
  logical :: stride_mask(3) = [.true., .false., .true.]

  print '(a,1x,i0)', 'min_fw', minloc(a, dim=1)
  print '(a,1x,i0)', 'min_back', minloc(a, dim=1, back=.true.)
  print '(a,1x,i0)', 'min_mask', minloc(a, dim=1, mask=m)
  print '(a,1x,i0)', 'min_mask_back', minloc(a, dim=1, mask=m, back=.true.)

  print '(a,1x,i0)', 'max_fw', maxloc(a, dim=1)
  print '(a,1x,i0)', 'max_back', maxloc(a, dim=1, back=.true.)
  print '(a,1x,i0)', 'max_mask', maxloc(a, dim=1, mask=m)
  print '(a,1x,i0)', 'max_false', maxloc(a, dim=1, mask=.false.)
  print '(a,1x,i0)', 'max_stride_back', &
    maxloc(a(1:5:2), dim=1, mask=stride_mask, back=.true.)
end program ar3_minmaxloc_mask_back
