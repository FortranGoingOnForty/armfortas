! STDERR_CHECK: REWIND: unit is not connected
! EXIT_CODE: 1
program rewind_unhandled_error
  implicit none

  rewind(123)
  error stop 1
end program rewind_unhandled_error
