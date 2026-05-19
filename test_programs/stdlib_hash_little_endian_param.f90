! Stdlib drill: stdlib_hash_32bit's public little_endian parameter is
! a TRANSFER-based initialization expression that must survive a real
! .amod/object boundary.
! MULTIFILE_LINK: endian_mod.f90 main.f90
! CHECK: ok
! REPRO_CHECK: run
!--- file: endian_mod.f90
module endian_mod
  use iso_fortran_env, only: int8, int16
  implicit none
  logical, parameter, public :: little_endian = &
    (1 == transfer([1_int8, 0_int8], 0_int16))
end module
!--- file: main.f90
program p
  use endian_mod, only: little_endian
  implicit none
  if (.not. little_endian) error stop 1
  print *, 'ok'
end program
