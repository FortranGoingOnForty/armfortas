! CHECK: ok
! IR_CHECK: const_int -2401053088876216593 : i64
! REPRO_CHECK: run
program stdlib_spooky_transfer_host_parameter
  use iso_fortran_env, only: int32, int64
  implicit none

  integer(int32), parameter :: sc_constsub = int(z'deadbeef', int32)
  integer(int64), parameter :: sc_const = transfer([sc_constsub, sc_constsub], 0_int64)

  call probe()
  print *, 'ok'

contains
  subroutine probe()
    if (sc_const /= -2401053088876216593_int64) error stop 1
  end subroutine probe
end program stdlib_spooky_transfer_host_parameter
