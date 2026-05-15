! CHECK: ok
! IR_CHECK: call @afs_create_section(
! IR_CHECK: call @memcpy(
! REPRO_CHECK: run
program complex_section_assign_preserves_imag
  implicit none
  integer, parameter :: dp = kind(0.0d0)
  complex(dp) :: src(5), dest(5, 3)

  src = [(0.57706_dp, 0.00000_dp), &
         (0.00000_dp, 1.44065_dp), &
         (1.26401_dp, 0.00000_dp), &
         (0.00000_dp, 0.88833_dp), &
         (1.14352_dp, 0.00000_dp)]

  dest = (0.0_dp, 0.0_dp)
  dest(:, 1) = src

  if (abs(real(dest(2, 1), kind=dp)) > 1.0e-12_dp) error stop 1
  if (abs(aimag(dest(2, 1)) - 1.44065_dp) > 1.0e-12_dp) error stop 2
  if (abs(real(dest(4, 1), kind=dp)) > 1.0e-12_dp) error stop 3
  if (abs(aimag(dest(4, 1)) - 0.88833_dp) > 1.0e-12_dp) error stop 4
  print *, 'ok'
end program
