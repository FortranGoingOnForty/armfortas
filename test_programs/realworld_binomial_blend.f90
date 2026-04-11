! stdlib/numerics-style small fixed-tap blend kernel.
! CHECK: 20 44 68 2072
! IR_CHECK: alloca [i32 x 4]
! IR_CHECK: rt_call @__afs_check_bounds
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_binomial_blend
  implicit none
  integer :: x(10), y(10), i, checksum

  do i = 1, 10
    x(i) = i
    y(i) = 0
  end do

  call blend(x, y)

  checksum = 0
  do i = 1, 10
    checksum = checksum + i * y(i)
  end do

  print *, y(3), y(6), y(9), checksum

contains

  subroutine blend(x, y)
    implicit none
    integer, intent(in) :: x(10)
    integer, intent(out) :: y(10)
    integer :: taps(4), i

    taps(1) = 1
    taps(2) = 3
    taps(3) = 3
    taps(4) = 1
    y(1) = 0
    y(2) = 0
    y(10) = 0

    do i = 3, 9
      y(i) = taps(1) * x(i - 2) + taps(2) * x(i - 1) + taps(3) * x(i) + taps(4) * x(i + 1)
    end do
  end subroutine blend
end program realworld_binomial_blend
