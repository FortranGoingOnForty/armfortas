! branchy classify/update kernel with noalias helper calls.
! CHECK: 4 52
! CHECK: 4 8 4 52
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_noalias_reuse
  implicit none
  integer, parameter :: n = 8
  integer :: x(n), y(n), total, hits, i

  do i = 1, n
    x(i) = i
  end do

  call classify_local(2, x, y, total)
  print *, y(2), total

  call classify_branchy(2, x, y, total, hits)
  print *, y(2), y(6), hits, total

contains

  recursive subroutine touch(counter)
    implicit none
    integer, intent(inout) :: counter

    if (counter < 0) then
      call touch(counter)
      return
    end if

    counter = counter + 1
  end subroutine touch

  recursive subroutine classify_local(bias, x, y, total)
    implicit none
    integer, intent(in) :: bias
    integer, intent(in) :: x(8)
    integer, intent(out) :: y(8)
    integer, intent(out) :: total
    integer :: i, scratch

    if (total < 0) then
      call classify_local(bias, x, y, total)
      return
    end if

    total = 0
    scratch = 0
    do i = 1, 8
      y(i) = x(i) + bias
      call touch(scratch)
      total = total + y(i)
    end do
  end subroutine classify_local

  recursive subroutine classify_branchy(bias, x, y, total, hits)
    implicit none
    integer, intent(in) :: bias
    integer, intent(in) :: x(8)
    integer, intent(out) :: y(8)
    integer, intent(out) :: total, hits
    integer :: i

    if (total < 0) then
      call classify_branchy(bias, x, y, total, hits)
      return
    end if

    total = 0
    hits = 0
    do i = 1, 8
      y(i) = x(i) + bias
      if (y(i) > 6) then
        call touch(hits)
      end if
      total = total + y(i)
    end do
  end subroutine classify_branchy
end program realworld_noalias_reuse
