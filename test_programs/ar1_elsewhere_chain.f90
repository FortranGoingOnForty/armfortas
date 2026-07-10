! CHECK: chain=99 -2 -3 40 50 60
! CHECK: original=77 77 0 0
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar1_elsewhere_chain
  implicit none

  integer :: a(6)
  integer :: b(4)

  a = [1, 2, 3, 4, 5, 6]
  where (a > 3)
    a = a * 10
  elsewhere (a > 1)
    a = -a
  elsewhere
    a = 99
  end where
  print '(a,6(i0,1x))', 'chain=', a

  b = [1, 2, 3, 4]
  where (b > 2)
    b = 0
  elsewhere (b(4:1:-1) == 0)
    b = 77
  elsewhere
    b = b * 10
  end where
  print '(a,4(i0,1x))', 'original=', b
end program ar1_elsewhere_chain
