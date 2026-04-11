! fpm-style branch-join tally kernel with repeated affine expressions.
! CHECK: 5 9 4 60
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_join_bias_sum
  implicit none
  integer, parameter :: n = 8
  integer :: y(n), z(n), total, hits

  call tally(y, z, total, hits)
  print *, y(2), z(6), hits, total

contains

  pure integer function offset_value(idx)
    implicit none
    integer, intent(in) :: idx

    offset_value = idx + 3
  end function offset_value

  subroutine tally(y, z, total, hits)
    implicit none
    integer, intent(out) :: y(8)
    integer, intent(out) :: z(8)
    integer, intent(out) :: total, hits
    integer :: i

    total = 0
    hits = 0
    do i = 1, 8
      if (offset_value(i) > 7) then
        hits = hits + 1
      end if
      y(i) = offset_value(i)
      z(i) = offset_value(i)
      total = total + z(i)
    end do
  end subroutine tally
end program realworld_join_bias_sum
