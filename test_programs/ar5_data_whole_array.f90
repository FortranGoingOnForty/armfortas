! Whole-array DATA object lists initialize each array element.
!
! CHECK: local 17 34 51 68 85 99
! CHECK: common 7 8 9 42
program ar5_data_whole_array
  implicit none
  integer :: a(5), x
  integer :: ca(3), marker
  common /blk/ ca, marker

  data a / 17, 34, 51, 68, 85 /
  data x / 99 /
  data ca / 7, 8, 9 /
  data marker / 42 /

  print '(a,5(i0,1x),i0)', 'local ', a, x
  call print_common()
contains
  subroutine print_common()
    implicit none
    integer :: alias(3), seen
    common /blk/ alias, seen

    print '(a,3(i0,1x),i0)', 'common ', alias, seen
  end subroutine print_common
end program ar5_data_whole_array
