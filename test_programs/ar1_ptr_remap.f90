! CHECK: omit=1 0 3 5 8
! CHECK: after_omit=1 2 3 4 5 6 200 8 9 10
! CHECK: explicit=1 0 3 5 8
! CHECK: after_explicit=1 2 3 4 5 111 7 8 9 10
! CHECK: stride=1 -2 1 2 8
! CHECK: after_stride=1 2 3 4 5 444 7 8 9 10
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar1_ptr_remap_m
contains
  subroutine bind_omitted_stride(r, v)
    integer, pointer, intent(out) :: r(:)
    integer, target, intent(inout) :: v(:)

    r(-2:) => v(2:8:2)
  end subroutine bind_omitted_stride
end module ar1_ptr_remap_m

program ar1_ptr_remap
  use ar1_ptr_remap_m
  implicit none

  integer, target :: v(10)
  integer, pointer :: r(:)
  integer :: associated_flag
  integer :: i

  v = [(i, i = 1, 10)]
  nullify(r)
  r(0:) => v(5:8)
  associated_flag = 0
  if (associated(r)) associated_flag = 1
  print '(a,5(i0,1x))', 'omit=', associated_flag, lbound(r, 1), ubound(r, 1), r(lbound(r, 1)), r(ubound(r, 1))
  r(2) = 200
  print '(a,10(i0,1x))', 'after_omit=', v

  v = [(i, i = 1, 10)]
  r => v(1:2)
  r(0:3) => v(5:8)
  associated_flag = 0
  if (associated(r)) associated_flag = 1
  print '(a,5(i0,1x))', 'explicit=', associated_flag, lbound(r, 1), ubound(r, 1), r(lbound(r, 1)), r(ubound(r, 1))
  r(1) = 111
  print '(a,10(i0,1x))', 'after_explicit=', v

  v = [(i, i = 1, 10)]
  call bind_omitted_stride(r, v)
  associated_flag = 0
  if (associated(r)) associated_flag = 1
  print '(a,5(i0,1x))', 'stride=', associated_flag, lbound(r, 1), ubound(r, 1), r(lbound(r, 1)), r(ubound(r, 1))
  r(0) = 444
  print '(a,10(i0,1x))', 'after_stride=', v
end program ar1_ptr_remap
