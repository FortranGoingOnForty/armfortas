! CHECK: ok
! REPRO_CHECK: run
program stdlib_transfer_array_operand_lanes
  use iso_fortran_env, only: int16, int32
  implicit none

  integer(int16) :: lhs(2), got(2)

  lhs = transfer(int(z'11223344', int32), 0_int16, 2)
  got = lhs * transfer(int(z'01020304', int32), 0_int16, 2)

  if (got(1) /= int(-26352, int16)) error stop 1
  if (got(2) /= int(17476, int16)) error stop 2

  print *, 'ok'
end program stdlib_transfer_array_operand_lanes
