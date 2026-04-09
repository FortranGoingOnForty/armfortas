! STDERR_CHECK: ERROR STOP
! EXIT_CODE: 1
! ASM_CHECK: bl _afs_error_stop
! ASM_NOT: bl _afs_stop
program error_stop_status
  error stop
end program error_stop_status
