! CHECK: swap1=2 3 4
! CHECK: swap2=2 1 2
! CHECK: rot1=2 20 21
! CHECK: rot2=3 30 31 32
! CHECK: rot3=1 10
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar1_dt_vecsub_mod
  implicit none

  type :: bag
    integer, allocatable :: v(:)
  end type bag
end module ar1_dt_vecsub_mod

program ar1_dt_vecsub
  use ar1_dt_vecsub_mod, only: bag
  implicit none

  type(bag) :: a(3)

  a(1)%v = [1, 2]
  a(2)%v = [3, 4]
  a = a([2, 1, 3])
  print '(a,3(i0,1x))', 'swap1=', size(a(1)%v), a(1)%v(1), a(1)%v(2)
  print '(a,3(i0,1x))', 'swap2=', size(a(2)%v), a(2)%v(1), a(2)%v(2)

  a(1)%v = [10]
  a(2)%v = [20, 21]
  a(3)%v = [30, 31, 32]
  a = a([2, 3, 1])
  print '(a,3(i0,1x))', 'rot1=', size(a(1)%v), a(1)%v(1), a(1)%v(2)
  print '(a,4(i0,1x))', 'rot2=', size(a(2)%v), a(2)%v(1), a(2)%v(2), a(2)%v(3)
  print '(a,2(i0,1x))', 'rot3=', size(a(3)%v), a(3)%v(1)
end program ar1_dt_vecsub
