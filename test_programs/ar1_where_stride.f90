! CHECK: rhs=4 3 2 1
! CHECK: self=4 3 2 1
! CHECK: construct=0 3 0 1
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar1_where_stride
  implicit none

  integer :: a(4), b(4)

  a = [1, 2, 3, 4]
  b = [1, 2, 3, 4]
  where (a > 0) a = b(4:1:-1)
  print '(a,4(i0,1x))', 'rhs=', a

  a = [1, 2, 3, 4]
  where (a > 0) a = a(4:1:-1)
  print '(a,4(i0,1x))', 'self=', a

  a = [0, 2, 0, 4]
  where (a > 0)
    a = b(4:1:-1)
  end where
  print '(a,4(i0,1x))', 'construct=', a
end program ar1_where_stride
