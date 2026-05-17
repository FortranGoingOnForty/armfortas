! CHECK: ok
! IR_CHECK: complex_part_check
! IR_CHECK: call @afs_create_section(
! REPRO_CHECK: run
program complex_part_section_expr_preserves_imag
  implicit none
  integer, parameter :: dp = kind(0.0d0)
  complex(dp) :: pval(3)
  real(dp) :: err(2), imv(2)

  pval(1) = cmplx(10.0_dp, 0.5_dp, kind=dp)
  pval(2) = cmplx(10.0_dp, 0.5_dp, kind=dp)
  pval(3) = cmplx(11.0_dp, 2.5_dp, kind=dp)

  imv = pval(2:3)%im
  err = sqrt((pval(2:3)%re - pval(1)%re)**2 + (pval(2:3)%im - pval(1)%im)**2)

  if (abs(imv(1) - 0.5_dp) > 1.0d-12) error stop 1
  if (abs(imv(2) - 2.5_dp) > 1.0d-12) error stop 2
  if (abs(err(1)) > 1.0d-12) error stop 3
  if (abs(err(2) - sqrt(5.0_dp)) > 1.0d-12) error stop 4

  print *, 'ok'
end program
