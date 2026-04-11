! stdlib iterative-solver-inspired three-point apply kernel.
! CHECK: 14 28 49 973
! IR_CHECK: alloca [i32 x 3]
! IR_CHECK: rt_call @__afs_check_bounds
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_three_point_apply
  implicit none
  integer :: x(8), y(8), alpha, beta, i, checksum

  alpha = 3
  beta = 2
  do i = 1, 8
    x(i) = i
    y(i) = 0
  end do

  call apply(alpha, beta, x, y)

  checksum = 0
  do i = 1, 8
    checksum = checksum + i * y(i)
  end do

  print *, y(2), y(4), y(7), checksum

contains

  subroutine apply(alpha, beta, x, y)
    implicit none
    integer, intent(in) :: alpha, beta
    integer, intent(in) :: x(8)
    integer, intent(out) :: y(8)
    integer :: coeffs(3), i

    coeffs(1) = beta
    coeffs(2) = alpha
    coeffs(3) = beta
    y(1) = 0
    y(8) = 0

    do i = 2, 7
      y(i) = coeffs(1) * x(i - 1) + coeffs(2) * x(i) + coeffs(3) * x(i + 1)
    end do
  end subroutine apply
end program realworld_three_point_apply
