! CHECK: pos=1001 2 1003 4 1005 6 1007 8 9 10
! CHECK: neg=1 2 3 104 5 106 7 108 9 110
! CHECK: in=15 v=1 2 3 4 5 6 7 8 9 10
! CHECK: local=1 12 3 4 15 6 7 18 9 10
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar1_secpack_m
contains
  subroutine bump_explicit(a, n, delta)
    integer, intent(in) :: n, delta
    integer, intent(inout) :: a(n)
    integer :: i

    do i = 1, n
      a(i) = a(i) + delta
    end do
  end subroutine bump_explicit

  subroutine bump_assumed_size(a, n, delta)
    integer, intent(in) :: n, delta
    integer, intent(inout) :: a(*)
    integer :: i

    do i = 1, n
      a(i) = a(i) + delta
    end do
  end subroutine bump_assumed_size

  subroutine sum_intent_in(a, n, total)
    integer, intent(in) :: n
    integer, intent(in) :: a(n)
    integer, intent(out) :: total
    integer :: i

    total = 0
    do i = 1, n
      total = total + a(i)
    end do
  end subroutine sum_intent_in
end module ar1_secpack_m

program ar1_secpack
  use ar1_secpack_m
  implicit none

  integer :: v(10)
  integer :: total

  call reset(v)
  call bump_explicit(v(1:7:2), 4, 1000)
  print '(a,10(i0,1x))', 'pos=', v

  call reset(v)
  call bump_assumed_size(v(10:4:-2), 4, 100)
  print '(a,10(i0,1x))', 'neg=', v

  call reset(v)
  call sum_intent_in(v(9:1:-4), 3, total)
  print '(a,i0,a,10(i0,1x))', 'in=', total, ' v=', v

  call reset(v)
  call local_bump(v(2:8:3), 3, 10)
  print '(a,10(i0,1x))', 'local=', v

contains
  subroutine reset(a)
    integer, intent(out) :: a(10)
    integer :: i

    do i = 1, 10
      a(i) = i
    end do
  end subroutine reset

  subroutine local_bump(a, n, delta)
    integer, intent(in) :: n, delta
    integer, intent(inout) :: a(n)
    integer :: i

    do i = 1, n
      a(i) = a(i) + delta
    end do
  end subroutine local_bump
end program ar1_secpack
