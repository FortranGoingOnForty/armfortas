! STDERR_CHECK: Fortran runtime error: unit not open for writing
! EXIT_CODE: 2
program write_namelist_unhandled_error
  implicit none

  integer :: unit, value
  namelist /group/ value

  value = 42
  open(newunit=unit, status='scratch', action='read')
  write(unit, nml=group)
  error stop 1
end program write_namelist_unhandled_error
