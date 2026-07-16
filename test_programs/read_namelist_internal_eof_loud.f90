! STDERR_CHECK: READ: end of file
! EXIT_CODE: 1
program read_namelist_internal_eof_loud
  implicit none

  character(len=16) :: record
  integer :: value
  namelist /wanted/ value

  record = ''
  read(record, nml=wanted)
  print *, 'survived'
end program read_namelist_internal_eof_loud
