! Real SUM/MAXVAL/MINVAL with a compile-time DIM and exactly one
! rank-preserving array section should consume that section descriptor
! directly instead of allocating an elemental-expression temporary.
! Covers nondefault bounds, dynamic bounds, positive/non-unit/negative
! strides, descriptor-backed sections, rank 3, real(4), and empty sections.
!
! CHECK: ok
! IR_CHECK: direct_sum_dim_check
! IR_CHECK: direct_maxval_dim_check
! IR_CHECK: direct_minval_dim_check
! IR_CHECK: call @afs_create_section(
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
program dim_section_elemental_direct_reduction
  implicit none
  real(8) :: x(-2:3, 4:8), z(2, 3, 2)
  real(8) :: rows(3), columns(3), square(2, 2)
  real(4) :: y(2, 4), singles(2), single_columns(4)
  integer :: i, j, k, first, last

  do j = lbound(x, 2), ubound(x, 2)
    do i = lbound(x, 1), ubound(x, 1)
      x(i, j) = real(10 * j + i, 8)
    end do
  end do
  first = -1
  last = 3

  rows = sum(x(first:last:2, 5:8)**2, dim=2)
  if (rows(1) /= 16884.0_8 .or. rows(2) /= 17924.0_8 .or. &
      rows(3) /= 18996.0_8) error stop 1
  columns = sum(abs(x(3:-2:-2, 8:4:-2)), dim=1)
  if (columns(1) /= 243.0_8 .or. columns(2) /= 183.0_8 .or. &
      columns(3) /= 123.0_8) error stop 2
  columns = maxval(abs(x(3:-2:-2, 8:4:-2)), dim=1)
  if (columns(1) /= 83.0_8 .or. columns(2) /= 63.0_8 .or. &
      columns(3) /= 43.0_8) error stop 3
  columns = minval(abs(x(3:-2:-2, 8:4:-2)), dim=1)
  if (columns(1) /= 79.0_8 .or. columns(2) /= 59.0_8 .or. &
      columns(3) /= 39.0_8) error stop 4

  y(:, 1) = [-2.0, 4.0]
  y(:, 2) = [1.0, 3.0]
  y(:, 3) = [-5.0, 6.0]
  y(:, 4) = [-7.0, 8.0]
  singles = sum((y(2:1:-1, 1:4:2) + 1.0)**2, dim=2)
  if (singles(1) /= 74.0 .or. singles(2) /= 17.0) error stop 5
  single_columns = maxval(abs(y(2:1:-1, 4:1:-1)), dim=1)
  if (single_columns(1) /= 8.0 .or. single_columns(2) /= 6.0 .or. &
      single_columns(3) /= 3.0 .or. single_columns(4) /= 4.0) error stop 6

  do k = 1, 2
    do j = 1, 3
      do i = 1, 2
        z(i, j, k) = real(100 * k + 10 * j + i, 8)
      end do
    end do
  end do
  square = sum(abs(z(2:1:-1, 1:3:2, 2:1:-1)), dim=2)
  if (square(1, 1) /= 444.0_8 .or. square(2, 1) /= 442.0_8 .or. &
      square(1, 2) /= 244.0_8 .or. square(2, 2) /= 242.0_8) error stop 7

  rows = sum(abs(x(-1:3:2, 9:8)), dim=2)
  if (rows(1) /= 0.0_8 .or. rows(2) /= 0.0_8 .or. &
      rows(3) /= 0.0_8) error stop 8
  rows = maxval(abs(x(-1:3:2, 9:8)), dim=2)
  if (rows(1) /= -huge(0.0_8) .or. rows(2) /= -huge(0.0_8) .or. &
      rows(3) /= -huge(0.0_8)) error stop 9
  rows = minval(abs(x(-1:3:2, 9:8)), dim=2)
  if (rows(1) /= huge(0.0_8) .or. rows(2) /= huge(0.0_8) .or. &
      rows(3) /= huge(0.0_8)) error stop 10

  call check_descriptor_section(x(3:-2:-2, 8:4:-2))
  print *, 'ok'

contains

  subroutine check_descriptor_section(a)
    real(8), intent(in) :: a(:, :)
    real(8) :: values(3)

    values = sum(a(3:1:-1, 1:3:2)**2, dim=2)
    if (values(1) /= 7762.0_8 .or. values(2) /= 8242.0_8 .or. &
        values(3) /= 8738.0_8) error stop 11
  end subroutine check_descriptor_section

end program dim_section_elemental_direct_reduction
