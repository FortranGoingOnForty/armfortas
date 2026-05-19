! CHECK: ok
! IR_CHECK: call @afs_create_section
! IR_CHECK: call @afs_allocate_like_with_elem_size
! IR_CHECK: call @afs_copy_array_data
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program rank_remap_strided_section_copyin
  implicit none

  real(8), target :: a(16)
  real(8), pointer :: twod(:,:)
  real(8) :: col(2), row(4)
  real(8) :: col_assumed, row_assumed
  real(8) :: col_explicit, row_explicit
  integer :: i

  do i = 1, 16
    a(i) = real(i, 8)
  end do

  twod(1:4, 1:4) => a
  col = twod(::2, 3)
  row = twod(3, :)

  if (any(abs(col - [9.0_8, 11.0_8]) > 1.0e-10_8)) error stop 1
  if (any(abs(row - [3.0_8, 7.0_8, 11.0_8, 15.0_8]) > 1.0e-10_8)) error stop 2

  col_assumed = assumed_sum(twod(::2, 3))
  row_assumed = assumed_sum(twod(3, :))
  call explicit_outer(twod(::2, 3), col_explicit)
  call explicit_outer(twod(3, :), row_explicit)

  if (abs(col_assumed - 20.0_8) > 1.0e-10_8) error stop 3
  if (abs(row_assumed - 36.0_8) > 1.0e-10_8) error stop 4
  if (abs(col_explicit - 20.0_8) > 1.0e-10_8) error stop 5
  if (abs(row_explicit - 36.0_8) > 1.0e-10_8) error stop 6

  print *, 'ok'

contains
  function assumed_sum(x) result(total)
    real(8), intent(in) :: x(:)
    real(8) :: total
    integer :: j

    total = 0.0_8
    do j = 1, size(x)
      total = total + x(j)
    end do
  end function assumed_sum

  subroutine explicit_outer(x, total)
    real(8), intent(in), target :: x(:)
    real(8), intent(out) :: total

    call explicit_inner(size(x), x, total)
  end subroutine explicit_outer

  subroutine explicit_inner(n, x, total)
    integer, intent(in) :: n
    real(8), intent(in) :: x(n)
    real(8), intent(out) :: total
    integer :: j

    total = 0.0_8
    do j = 1, n
      total = total + x(j)
    end do
  end subroutine explicit_inner
end program rank_remap_strided_section_copyin
