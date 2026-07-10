! CHECK: 3
! CHECK: 3 4 T tag 9 hi 5 6
! CHECK: ok
! IR_CHECK: call @afs_write_int
! IR_CHECK: call @afs_fmt_push_int
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_derived_scalar_io
  implicit none

  type :: inner
    integer :: k = 9
    character(len=2) :: code = 'hi'
  end type inner

  type :: pair
    integer :: i = 3
    integer :: j = 4
    logical :: ok = .true.
    character(len=3) :: tag = 'tag'
    type(inner) :: child
    integer :: vals(2) = [5, 6]
  end type pair

  type(pair) :: x

  x%vals = [5, 6]
  print *, x
  write (*, '(i0,1x,i0,1x,l1,1x,a,1x,i0,1x,a,1x,i0,1x,i0)') x
  print '(a)', 'ok'
end program ar10_derived_scalar_io
