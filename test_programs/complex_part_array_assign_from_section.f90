! CHECK: ok
! IR_CHECK: call @afs_create_section(
! IR_CHECK: complex_part_assign_check
! REPRO_CHECK: run
program complex_part_array_assign_from_section
  implicit none
  integer, parameter :: dp = kind(0.0d0)
  real(dp) :: z(4, 2)
  complex(dp) :: x(4)
  integer :: i

  do i = 1, 4
    z(i, 1) = real(i, dp)
    z(i, 2) = real(i + 10, dp)
  end do

  x = cmplx(0.0_dp, 0.0_dp, kind=dp)
  x%re = z(:, 1)
  x%im = z(:, 2)

  do i = 1, 4
    if (abs(real(x(i), kind=dp) - z(i, 1)) > 1.0d-12) error stop 10 + i
    if (abs(aimag(x(i)) - z(i, 2)) > 1.0d-12) error stop 20 + i
  end do

  print *, 'ok'
end program
