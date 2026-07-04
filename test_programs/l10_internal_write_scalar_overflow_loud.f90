! l10: scalar internal-file overflow without IOSTAT= is loud.
! STDERR_CHECK: more than one record
! EXIT_CODE: 2
program l10_internal_write_scalar_overflow_loud
  implicit none
  character(len=20) :: s
  write(s, '(i0)') 1, 2
  print '(a)', 'unreachable'
end program l10_internal_write_scalar_overflow_loud
