! CHECK: first=3 6 9 sum=18
! CHECK: after=1 2 3 4 5 100 7 8 9 10 11 12
! CHECK: neg=10 7 4 total=21
! CHECK: after_neg=1 2 3 333 5 100 7 8 9 10 11 12
! CHECK: proc=4 8 12 sum=24
! CHECK: after_proc=1 2 3 444 5 6 7 8 9 10 11 12
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar1_ptr_stride_m
contains
  subroutine bind_stride(q, v)
    integer, pointer, intent(out) :: q(:)
    integer, target, intent(inout) :: v(:)

    q => v(4:12:4)
  end subroutine bind_stride
end module ar1_ptr_stride_m

program ar1_ptr_stride
  use ar1_ptr_stride_m
  implicit none

  integer, target :: v(12)
  integer, pointer :: q(:)
  integer :: i

  v = [(i, i = 1, 12)]
  q => v(3:9:3)
  print '(a,3(i0,1x),a,i0)', 'first=', q(1), q(2), q(3), 'sum=', sum(q)
  q(2) = 100
  print '(a,12(i0,1x))', 'after=', v

  q => v(10:4:-3)
  print '(a,3(i0,1x),a,i0)', 'neg=', q(1), q(2), q(3), 'total=', q(1) + q(2) + q(3)
  q(3) = 333
  print '(a,12(i0,1x))', 'after_neg=', v

  v = [(i, i = 1, 12)]
  call bind_stride(q, v)
  print '(a,3(i0,1x),a,i0)', 'proc=', q(1), q(2), q(3), 'sum=', sum(q)
  q(1) = 444
  print '(a,12(i0,1x))', 'after_proc=', v
end program ar1_ptr_stride
