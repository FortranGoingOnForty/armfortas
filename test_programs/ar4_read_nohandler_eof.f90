! EXIT_CODE: 1
! STDERR_CHECK: READ: end of file
program ar4_read_nohandler_eof
  implicit none
  integer :: u, x

  open(newunit=u, status='scratch', action='readwrite', form='formatted')
  rewind(u)
  x = 123
  read(u, '(i4)') x
  print '(a)', 'nohandler_missed'
end program
