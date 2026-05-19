! CHECK: ok
! IR_CHECK: call @afs_cpu_time(
! REPRO_CHECK: run
program stdlib_cpu_time_default_real
  implicit none
  real :: t32
  real(8) :: t64

  call cpu_time(t32)
  call cpu_time(t64)

  if (t32 < 0.0) error stop 1
  if (t64 < 0.0_8) error stop 2

  print *, 'ok'
end program
