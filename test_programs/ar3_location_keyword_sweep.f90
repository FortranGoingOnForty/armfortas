program ar3_location_keyword_sweep
  implicit none

  integer :: a(2,3)
  logical :: m(2,3)
  real(8) :: r(2,3)

  a = reshape([1, 9, 8, 5, 7, 7], [2,3])
  m = reshape([.false., .true., .true., .false., .true., .true.], [2,3])
  r = real(a, kind=8)

  print '(a,3(1x,i0))', 'maxloc_dim_mask', maxloc(a, dim=1, mask=m)
  print '(a,3(1x,i0))', 'maxloc_dim_mask_back', maxloc(a, dim=1, mask=m, back=.true.)
  print '(a,3(1x,i0))', 'minloc_dim_mask_back', minloc(a, dim=1, mask=m, back=.true.)
  print '(a,3(1x,i0))', 'findloc_dim_mask_back', findloc(a, 7, dim=1, mask=m, back=.true.)
  print '(a,3(1x,i0))', 'findloc_real_dim_mask_back', findloc(r, 7.0_8, dim=1, mask=m, back=.true.)
  print '(a,3(1x,i0))', 'findloc_logical_dim_back', findloc(m, .true., dim=1, back=.true.)
  print '(a,3(1x,i0))', 'findloc_scalar_false', findloc(a, 7, dim=1, mask=.false.)
end program
! CHECK: maxloc_dim_mask 2 1 1
! CHECK: maxloc_dim_mask_back 2 1 2
! CHECK: minloc_dim_mask_back 2 1 2
! CHECK: findloc_dim_mask_back 0 0 2
! CHECK: findloc_real_dim_mask_back 0 0 2
! CHECK: findloc_logical_dim_back 2 1 2
! CHECK: findloc_scalar_false 0 0 0
