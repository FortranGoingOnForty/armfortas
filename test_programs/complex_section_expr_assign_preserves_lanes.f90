! CHECK: ok
! IR_CHECK: call @afs_create_section(
! IR_CHECK: call @afs_array_conjg(
! IR_CHECK: call @memcpy(
! REPRO_CHECK: run
program complex_section_expr_assign_preserves_lanes
  implicit none
  integer, parameter :: dp = kind(0.0d0)
  complex(dp) :: a(2), b(2), c(2), d(2), expected(2), fill

  a(1) = cmplx(1.0_dp, 2.0_dp, kind=dp)
  a(2) = cmplx(3.0_dp, -4.0_dp, kind=dp)
  b(1) = cmplx(5.0_dp, 7.0_dp, kind=dp)
  b(2) = cmplx(-2.0_dp, 11.0_dp, kind=dp)
  expected(1) = cmplx(19.0_dp, -3.0_dp, kind=dp)
  expected(2) = cmplx(-50.0_dp, 25.0_dp, kind=dp)

  c(1:2) = conjg(a(1:2)) * b(1:2)
  fill = cmplx(7.0_dp, -9.0_dp, kind=dp)
  d(1:2) = fill

  if (abs(real(c(1), kind=dp) - real(expected(1), kind=dp)) > 1.0d-12) error stop 1
  if (abs(aimag(c(1)) - aimag(expected(1))) > 1.0d-12) error stop 2
  if (abs(real(c(2), kind=dp) - real(expected(2), kind=dp)) > 1.0d-12) error stop 3
  if (abs(aimag(c(2)) - aimag(expected(2))) > 1.0d-12) error stop 4
  if (abs(real(d(2), kind=dp) - 7.0_dp) > 1.0d-12) error stop 5
  if (abs(aimag(d(2)) + 9.0_dp) > 1.0d-12) error stop 6

  print *, 'ok'
end program
