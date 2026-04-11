! fpm-style classify-and-accumulate loop with branchy output reuse.
! CHECK: 4 8 4 52
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_branch_sum
  implicit none
  integer, parameter :: n = 8
  integer :: x(n), y(n), total, hits, i

  do i = 1, n
    x(i) = i
  end do

  call classify(2, x, y, total, hits)
  print *, y(2), y(6), hits, total

contains

  recursive subroutine classify(bias, x, y, total, hits)
    implicit none
    integer, intent(in) :: bias
    integer, intent(in) :: x(8)
    integer, intent(out) :: y(8)
    integer, intent(out) :: total, hits
    integer :: i

    if (total < 0) then
      call classify(bias, x, y, total, hits)
      return
    end if

    total = 0
    hits = 0
    do i = 1, 8
      y(i) = x(i) + bias
      if (y(i) > 6) then
        hits = hits + 1
      end if
      total = total + y(i)
    end do
  end subroutine classify
end program realworld_branch_sum
