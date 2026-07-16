! STDERR_CHECK: READ: end of file
! EXIT_CODE: 1
program read_namelist_eof_loud
  implicit none

  character(len=*), parameter :: path = 'read_namelist_eof_loud.tmp'
  integer :: unit, value
  namelist /wanted/ value

  open(newunit=unit, file=path, status='replace', action='write')
  close(unit)
  open(newunit=unit, file=path, status='old', action='read')
  read(unit, nml=wanted)
  print *, 'survived'
end program read_namelist_eof_loud
