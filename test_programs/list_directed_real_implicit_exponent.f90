! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: obj|repro
program list_directed_real_implicit_exponent
  implicit none
  real :: spv
  double precision :: dpv
  character(len=64) :: s
  integer :: fails

  fails = 0

  s = "1.-3"
  read(s,*) spv
  if (abs(spv - 1.0e-3) > 1.0e-8) fails = fails + 1

  s = "1.+3"
  read(s,*) spv
  if (abs(spv - 1.0e3) > 1.0e-3) fails = fails + 1

  s = "1234567890-9"
  read(s,*) dpv
  if (abs(dpv - 1.234567890d0) > 1.0d-12) fails = fails + 1

  s = "123456.789+2"
  read(s,*) dpv
  if (abs(dpv - 12345678.9d0) > 1.0d-6) fails = fails + 1

  if (fails /= 0) then
    print *, "fail"
    error stop
  end if

  print *, "ok"
end program
