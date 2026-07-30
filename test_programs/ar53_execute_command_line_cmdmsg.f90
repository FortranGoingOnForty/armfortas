! EXECUTE_COMMAND_LINE must define CMDMSG when command validation or
! process creation fails, while leaving it unchanged after successful start.
!
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_execute_command_line_cmdmsg(
program ar53_execute_command_line_cmdmsg
  implicit none

  integer :: cmdstat, exitstat
  character(len=64) :: cmdmsg
  character(len=8) :: short_messages(2)
  character(len=1) :: invalid_command

  cmdmsg = 'unchanged'
  cmdstat = -99
  exitstat = -99
  call execute_command_line( &
    'exit 0', cmdmsg=cmdmsg, exitstat=exitstat, cmdstat=cmdstat)
  if (cmdstat /= 0 .or. exitstat /= 0) error stop 1
  if (trim(cmdmsg) /= 'unchanged') error stop 2

  invalid_command = achar(0)
  cmdmsg = 'unchanged'
  cmdstat = 0
  exitstat = -99
  call execute_command_line( &
    invalid_command, .true., exitstat, cmdstat, cmdmsg)
  if (cmdstat == 0) error stop 3
  if (len_trim(cmdmsg) == 0 .or. trim(cmdmsg) == 'unchanged') error stop 4
  if (exitstat /= -99) error stop 5

  short_messages = 'XXXXXXXX'
  cmdstat = 0
  call execute_command_line( &
    command=invalid_command, cmdmsg=short_messages(2), &
    wait=.false., cmdstat=cmdstat)
  if (cmdstat == 0) error stop 6
  if (short_messages(1) /= 'XXXXXXXX') error stop 7
  if (short_messages(2) == 'XXXXXXXX') error stop 8

  print '(a)', 'ok'
end program ar53_execute_command_line_cmdmsg
