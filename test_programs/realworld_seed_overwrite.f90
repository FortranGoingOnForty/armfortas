! staged output kernel with dead seed stores before the real fill.
! CHECK: 4 19 23
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_seed_overwrite
  implicit none
  integer, parameter :: n = 24
  integer :: x(n), y(n), total, i

  do i = 1, n
    x(i) = i
  end do

  call seed_and_fill(2, x, y, total)
  print *, y(2), y(17), total

contains

  subroutine touch(counter)
    implicit none
    integer, intent(inout) :: counter

    counter = counter + 1
  end subroutine touch

  recursive subroutine seed_and_fill(bias, x, y, total)
    implicit none
    integer, intent(in) :: bias
    integer, intent(in) :: x(n)
    integer, intent(out) :: y(n)
    integer, intent(out) :: total
    integer :: i, token

    if (bias < 0) then
      call seed_and_fill(bias, x, y, total)
      return
    end if

    token = 0
    do i = 1, n
      y(i) = 0
      call touch(token)
      y(i) = x(i) + bias
    end do

    total = y(2) + y(17)
  end subroutine seed_and_fill
end program realworld_seed_overwrite
