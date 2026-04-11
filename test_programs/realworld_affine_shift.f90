! stdlib-style affine update kernel with invariant scalar dummies.
! CHECK: 14 16
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_affine_shift
  implicit none
  integer, parameter :: n = 8
  integer :: x(n), y(n), total, i, alpha, shift

  do i = 1, n
    x(i) = i
  end do

  alpha = 2
  shift = 5
  call apply(alpha, shift, x, y, total)

  alpha = 3
  shift = 2
  call apply(alpha, shift, x, y, total)

  print *, y(4), total

contains

  recursive subroutine apply(alpha, shift, x, y, total)
    implicit none
    integer, intent(in) :: alpha, shift
    integer, intent(in) :: x(8)
    integer, intent(out) :: y(8)
    integer, intent(out) :: total
    integer :: i

    if (total < 0) then
      call apply(alpha, shift, x, y, total)
      return
    end if

    total = 0
    do i = 1, 8
      y(i) = alpha * x(i) + shift
      total = total + shift
    end do
  end subroutine apply
end program realworld_affine_shift
