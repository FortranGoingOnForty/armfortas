! STDERR_CHECK: Fortran runtime error:
! EXIT_CODE: 2
program write_pos_failure_loud
  implicit none

  integer :: unit

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, *, pos=0) 'discarded'
  print *, 'survived'
end program write_pos_failure_loud
