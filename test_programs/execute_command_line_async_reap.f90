! Repeated asynchronous commands must return promptly and be reaped while
! the parent process remains alive.
! CHECK: async-commands T
program execute_command_line_async_reap
  implicit none
  integer :: cmdstat, i
  logical :: launched

  launched = .true.
  do i = 1, 64
    call execute_command_line('exit 0', wait=.false., cmdstat=cmdstat)
    launched = launched .and. cmdstat == 0
  end do

  call execute_command_line('sleep 1')
  print *, 'async-commands', launched
end program execute_command_line_async_reap
