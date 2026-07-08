! COMMON array members keep their declared shape for whole-array
! references, element references, intrinsics, and positional aliases.
!
! CHECK: whole 7 8 9
! CHECK: element 7 9
! CHECK: sum 24 41
! CHECK: nested 80 96
! CHECK: alias 7 80 9 42
program ar5_common_array
  implicit none
  integer :: ia(3), scalar
  common /blk/ ia, scalar

  ia = [7, 8, 9]
  scalar = 41
  print '(a,3(i0,1x))', 'whole ', ia
  print '(a,i0,1x,i0)', 'element ', ia(1), ia(3)
  print '(a,i0,1x,i0)', 'sum ', sum(ia), scalar

  call mutate()
  print '(a,3(i0,1x),i0)', 'alias ', ia, scalar

contains
  subroutine mutate()
    implicit none
    integer :: x(3), y
    common /blk/ x, y
    x(2) = 80
    y = y + 1
    print '(a,i0,1x,i0)', 'nested ', x(2), sum(x)
  end subroutine mutate
end program ar5_common_array
