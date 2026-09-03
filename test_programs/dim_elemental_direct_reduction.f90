! Real SUM/MAXVAL/MINVAL with a compile-time DIM and one inert whole-array
! control should evaluate the elemental expression inside the reduction loop.
! The fixture covers fixed and descriptor-backed arrays, non-unit/negative
! physical strides, nondefault lower bounds, rank 3, real(4), and both forms
! of an empty reduced dimension.
!
! CHECK: ok
! IR_CHECK: direct_sum_dim_check
! IR_CHECK: direct_maxval_dim_check
! IR_CHECK: direct_minval_dim_check
! IR_NOT: call @afs_array_sum_real8_dim(
! IR_NOT: call @afs_array_maxval_real8_dim(
! IR_NOT: call @afs_array_minval_real8_dim(
! IR_NOT: call @afs_allocate_like(
! IR_NOT: call @afs_allocate_like_with_elem_size(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program dim_elemental_direct_reduction
  implicit none
  integer, parameter :: d1 = 1
  real(8) :: x(-1:1, 2:3), z(2, 2, 2)
  real(8) :: pair(2), triple(3), square(2, 2)
  real(4) :: y(2, 3), single_pair(2), single_triple(3)
  real(8), allocatable :: empty_first(:, :), empty_second(:, :)

  x(-1, 2) = -6.0_8
  x( 0, 2) = -1.0_8
  x( 1, 2) =  4.0_8
  x(-1, 3) =  2.0_8
  x( 0, 3) = -3.0_8
  x( 1, 3) =  5.0_8

  pair = sum(abs(x), dim=d1)
  if (pair(1) /= 11.0_8 .or. pair(2) /= 10.0_8) error stop 1
  triple = sum(abs(x), dim=2)
  if (triple(1) /= 8.0_8 .or. triple(2) /= 4.0_8 .or. &
      triple(3) /= 9.0_8) error stop 2
  pair = sum((x + 1.0_8)**2, dim=1)
  if (pair(1) /= 50.0_8 .or. pair(2) /= 49.0_8) error stop 3
  pair = maxval(abs(x), dim=1)
  if (pair(1) /= 6.0_8 .or. pair(2) /= 5.0_8) error stop 4
  triple = minval(abs(x), dim=2)
  if (triple(1) /= 2.0_8 .or. triple(2) /= 1.0_8 .or. &
      triple(3) /= 4.0_8) error stop 5

  y(1, 1) = -2.0
  y(2, 1) =  4.0
  y(1, 2) =  1.0
  y(2, 2) =  3.0
  y(1, 3) = -5.0
  y(2, 3) =  6.0
  single_triple = sum(abs(y), dim=1)
  if (single_triple(1) /= 6.0 .or. single_triple(2) /= 4.0 .or. &
      single_triple(3) /= 11.0) error stop 6
  single_pair = maxval(abs(y), dim=2)
  if (single_pair(1) /= 5.0 .or. single_pair(2) /= 6.0) error stop 7
  single_pair = minval(abs(y), dim=2)
  if (single_pair(1) /= 1.0 .or. single_pair(2) /= 3.0) error stop 8

  z(1, 1, 1) = -1.0_8
  z(2, 1, 1) =  2.0_8
  z(1, 2, 1) = -3.0_8
  z(2, 2, 1) =  4.0_8
  z(1, 1, 2) = -5.0_8
  z(2, 1, 2) =  6.0_8
  z(1, 2, 2) = -7.0_8
  z(2, 2, 2) =  8.0_8
  square = sum(abs(z), dim=2)
  if (square(1, 1) /= 4.0_8 .or. square(2, 1) /= 6.0_8 .or. &
      square(1, 2) /= 12.0_8 .or. square(2, 2) /= 14.0_8) error stop 9

  call check_strided(x(1:-1:-1, 3:2:-1))

  allocate(empty_first(0, 3), empty_second(2, 0))
  triple = sum(abs(empty_first), dim=1)
  if (triple(1) /= 0.0_8 .or. triple(2) /= 0.0_8 .or. &
      triple(3) /= 0.0_8) error stop 14
  triple = maxval(abs(empty_first), dim=1)
  if (triple(1) /= -huge(0.0_8) .or. triple(2) /= -huge(0.0_8) .or. &
      triple(3) /= -huge(0.0_8)) error stop 15
  triple = minval(abs(empty_first), dim=1)
  if (triple(1) /= huge(0.0_8) .or. triple(2) /= huge(0.0_8) .or. &
      triple(3) /= huge(0.0_8)) error stop 16
  pair = sum(abs(empty_second), dim=2)
  if (pair(1) /= 0.0_8 .or. pair(2) /= 0.0_8) error stop 17
  pair = maxval(abs(empty_second), dim=2)
  if (pair(1) /= -huge(0.0_8) .or. pair(2) /= -huge(0.0_8)) error stop 18
  pair = minval(abs(empty_second), dim=2)
  if (pair(1) /= huge(0.0_8) .or. pair(2) /= huge(0.0_8)) error stop 19

  print *, 'ok'

contains

  subroutine check_strided(a)
    real(8), intent(in) :: a(:, :)
    real(8) :: columns(2), rows(3)

    columns = sum(abs(a), dim=1)
    if (columns(1) /= 10.0_8 .or. columns(2) /= 11.0_8) error stop 10
    rows = sum(abs(a), dim=2)
    if (rows(1) /= 9.0_8 .or. rows(2) /= 4.0_8 .or. &
        rows(3) /= 8.0_8) error stop 11
    columns = maxval(abs(a), dim=1)
    if (columns(1) /= 5.0_8 .or. columns(2) /= 6.0_8) error stop 12
    rows = minval(abs(a), dim=2)
    if (rows(1) /= 4.0_8 .or. rows(2) /= 1.0_8 .or. &
        rows(3) /= 2.0_8) error stop 13
  end subroutine check_strided

end program dim_elemental_direct_reduction
