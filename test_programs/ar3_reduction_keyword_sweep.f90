program ar3_reduction_keyword_sweep
  implicit none

  integer :: a(2,3)
  logical :: m(2,3)
  real(8) :: r(2,3)

  a = reshape([1, 9, 8, 5, 7, 7], [2,3])
  m = reshape([.false., .true., .true., .false., .true., .true.], [2,3])
  r = real(a, kind=8)

  print '(a,3(1x,i0))', 'sum_dim_mask', sum(a, dim=1, mask=m)
  print '(a,3(1x,i0))', 'product_dim', product(a, dim=1)
  print '(a,3(1x,i0))', 'product_dim_mask', product(a, dim=1, mask=m)
  print '(a,3(1x,i0))', 'maxval_dim_mask', maxval(a, dim=1, mask=m)
  print '(a,3(1x,i0))', 'minval_dim_mask', minval(a, dim=1, mask=m)
  print '(a,3(1x,l1))', 'any_dim', any(m, dim=1)
  print '(a,3(1x,l1))', 'all_dim', all(m, dim=1)
  print '(a,3(1x,i0))', 'count_dim', count(m, dim=1)

  if (any(abs(product(r, dim=1) - [9.0_8, 40.0_8, 49.0_8]) > 1.0e-9_8)) error stop 1
  if (any(abs(product(r, dim=1, mask=m) - [9.0_8, 8.0_8, 49.0_8]) > 1.0e-9_8)) error stop 2
  if (any(abs(maxval(r, dim=1, mask=m) - [9.0_8, 8.0_8, 7.0_8]) > 1.0e-9_8)) error stop 3
  if (any(abs(minval(r, dim=1, mask=m) - [9.0_8, 8.0_8, 7.0_8]) > 1.0e-9_8)) error stop 4
  print '(a)', 'real_dim_mask ok'
end program
! CHECK: sum_dim_mask 9 8 14
! CHECK: product_dim 9 40 49
! CHECK: product_dim_mask 9 8 49
! CHECK: maxval_dim_mask 9 8 7
! CHECK: minval_dim_mask 9 8 7
! CHECK: any_dim T T T
! CHECK: all_dim F F T
! CHECK: count_dim 1 1 2
! CHECK: real_dim_mask ok
