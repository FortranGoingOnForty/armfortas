! STDERR_CHECK: Fortran runtime error:
! EXIT_CODE: 2
program write_list_error_loud
  implicit none

  character(len=*), parameter :: path = 'write_list_error_loud.tmp'
  integer :: unit

  open(newunit=unit, file=path, status='replace', action='write')
  close(unit)
  open(newunit=unit, file=path, status='old', action='read')
  write(unit, *) 42
  print *, 'survived'
end program write_list_error_loud
