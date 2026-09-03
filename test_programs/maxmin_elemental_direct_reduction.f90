! No-DIM real MAXVAL/MINVAL over an inert elemental expression should fold
! that expression directly into a rank-N descriptor-stride-aware reduction.
! Plain-array reductions retain the runtime path, as do DIM/MASK forms tested
! by maxval_minval_abs_dim_reduction.f90.
!
! CHECK: ok
! IR_CHECK: direct_maxval_check
! IR_CHECK: direct_minval_check
! IR_CHECK: call @afs_array_maxval_real8(
! IR_CHECK: call @afs_array_minval_real8(
! IR_NOT: call @afs_allocate_like(
! IR_NOT: call @afs_allocate_like_with_elem_size(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program maxmin_elemental_direct_reduction
  implicit none
  real(8) :: x(-1:1, 2:3)
  real :: single(2, 2)
  real :: empty(0, 2)

  x(-1, 2) = -6.0_8
  x( 0, 2) = -1.0_8
  x( 1, 2) =  4.0_8
  x(-1, 3) =  2.0_8
  x( 0, 3) = -3.0_8
  x( 1, 3) =  5.0_8
  single(1, 1) = -1.0
  single(2, 1) =  2.0
  single(1, 2) = -3.0
  single(2, 2) =  4.0

  if (maxval(abs(x)) /= 6.0_8) error stop 1
  if (minval(abs(x + 1.0_8)) /= 0.0_8) error stop 2
  if (maxval(abs(single)) /= 4.0) error stop 3
  if (maxval(abs(empty)) /= -huge(0.0)) error stop 4
  if (minval(abs(empty)) /= huge(0.0)) error stop 5
  if (maxval(x) /= 5.0_8) error stop 6
  if (minval(x) /= -6.0_8) error stop 7

  call check_strided(x(1:-1:-1, 3:2:-1))
  print *, 'ok'

contains

  subroutine check_strided(a)
    real(8), intent(in) :: a(:, :)

    if (maxval(abs(a)) /= 6.0_8) error stop 8
    if (minval(abs(a)) /= 1.0_8) error stop 9
  end subroutine check_strided
end program maxmin_elemental_direct_reduction
