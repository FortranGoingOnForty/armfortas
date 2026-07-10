! CHECK: rank1=4 3 2 1
! CHECK: rank2=2 1 4 3 6 5
! CHECK: masked=1 3 2 1
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar1_forall_overlap
  implicit none

  integer :: a(4)
  integer :: m(2,3)
  integer :: i, j

  a = [1, 2, 3, 4]
  forall (i = 1:4) a(i) = a(5 - i)
  print '(a,4(i0,1x))', 'rank1=', a

  m = reshape([1, 2, 3, 4, 5, 6], shape(m))
  forall (i = 1:2, j = 1:3) m(i,j) = m(3 - i,j)
  print '(a,6(i0,1x))', 'rank2=', m

  a = [1, 2, 3, 4]
  forall (i = 1:4, a(i) > 1) a(i) = a(5 - i)
  print '(a,4(i0,1x))', 'masked=', a
end program ar1_forall_overlap
