! SUM over a side-effect-free rank-1 real elemental expression should fold the
! element expression directly into the reduction loop. The assumed-shape
! dummy receives a descending-stride section, while LOWERED exercises a fixed
! array whose lower bound is not one. Empty arrays retain SUM's zero identity.
!
! CHECK: ok
! IR_CHECK: direct_sum_check
! IR_CHECK: call @afs_array_sum_real8(
! IR_NOT: call @afs_allocate_like_with_elem_size(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program sum_elemental_direct_reduction
  implicit none
  integer :: i
  real :: single(4)
  real(8) :: storage(12), lowered(-2:2)

  do i = 1, size(storage)
    storage(i) = real(i - 7, 8)
  end do
  do i = lbound(lowered, 1), ubound(lowered, 1)
    lowered(i) = real(i, 8)
  end do
  single = [1.0, 2.0, 3.0, 4.0]

  call check_strided(storage(12:2:-2), 2.0_8)
  call check_empty(storage(2:1), 2.0_8)

  if (sum((lowered / 2.0_8)**2) /= 2.5_8) error stop 7
  if (sum(single**2) /= 30.0) error stop 8

  print *, "ok"

contains

  subroutine check_strided(x, scaling)
    real(8), intent(in) :: x(:), scaling

    if (sum((x / scaling)**2) /= 17.5_8) error stop 1
    if (sum(abs(x)) /= 18.0_8) error stop 2
    if (sum(abs(x)**3) /= 306.0_8) error stop 3
    if (sum(x * x) /= 70.0_8) error stop 4
    if (sum(-x) /= 0.0_8) error stop 5
    if (sum(x) /= 0.0_8) error stop 6
  end subroutine check_strided

  subroutine check_empty(x, scaling)
    real(8), intent(in) :: x(:), scaling

    if (sum((x / scaling)**2) /= 0.0_8) error stop 9
    if (sum(abs(x)) /= 0.0_8) error stop 10
  end subroutine check_empty
end program sum_elemental_direct_reduction
