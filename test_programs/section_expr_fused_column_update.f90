! CHECK: ok
! IR_CHECK: md_section_check
! IR_NOT: call @afs_allocate_like_with_elem_size
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program section_expr_fused_column_update
  implicit none
  real(8) :: x(3, 2), y(2, 2), z(3, 2)
  integer :: i, j

  x(1, 1) = 1.0_8
  x(2, 1) = 2.0_8
  x(3, 1) = 3.0_8
  x(1, 2) = 4.0_8
  x(2, 2) = 5.0_8
  x(3, 2) = 6.0_8
  y(1, 1) = 2.0_8
  y(2, 1) = 3.0_8
  y(1, 2) = -1.0_8
  y(2, 2) = 4.0_8
  z = 0.0_8

  do j = 1, 2
    do i = 1, 2
      z(:, j) = z(:, j) + x(:, i) * y(i, j)
    end do
  end do

  if (z(1, 1) /= 14.0_8) error stop 1
  if (z(2, 1) /= 19.0_8) error stop 2
  if (z(3, 1) /= 24.0_8) error stop 3
  if (z(1, 2) /= 15.0_8) error stop 4
  if (z(2, 2) /= 18.0_8) error stop 5
  if (z(3, 2) /= 21.0_8) error stop 6
  print *, "ok"
end program section_expr_fused_column_update
