! EXECUTE_COMMAND_LINE uses the platform command processor even when PATH
! cannot resolve "sh". Exercise both synchronous and asynchronous launches.
!
! FLAGS: --std=f2018
! CHECK: ok
! FILE_CHECK: ar53_shell_sync.txt => sync
! FILE_CHECK: ar53_shell_async.txt => async
! FILE_SET_EXACT: ar53_shell_sync.txt,ar53_shell_async.txt
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|clean|repro
! IR_CHECK: call @afs_execute_command_line_cmdmsg(
program ar53_execute_command_line_path
  use iso_c_binding, only: c_char, c_int, c_null_char
  implicit none

  interface
    function c_setenv(name, value, overwrite) bind(C, name='setenv') result(status)
      import :: c_char, c_int
      character(kind=c_char), intent(in) :: name(*)
      character(kind=c_char), intent(in) :: value(*)
      integer(c_int), value :: overwrite
      integer(c_int) :: status
    end function c_setenv
  end interface

  character(kind=c_char, len=5) :: path_name
  character(kind=c_char, len=64) :: missing_path
  character(len=64) :: cmdmsg
  integer :: cmdstat, exitstat

  path_name = 'PATH' // c_null_char
  missing_path = '/armfortas-command-path-does-not-exist' // c_null_char
  if (c_setenv(path_name, missing_path, 1_c_int) /= 0_c_int) error stop 1

  cmdmsg = 'unchanged'
  cmdstat = -1
  exitstat = -1
  call execute_command_line( &
    'printf ''sync\n'' > ar53_shell_sync.txt', &
    exitstat=exitstat, cmdstat=cmdstat, cmdmsg=cmdmsg)
  if (cmdstat /= 0 .or. exitstat /= 0) error stop 2
  if (trim(cmdmsg) /= 'unchanged') error stop 3

  cmdmsg = 'unchanged'
  cmdstat = -1
  call execute_command_line( &
    'printf ''async\n'' > ar53_shell_async.txt', wait=.false., &
    cmdstat=cmdstat, cmdmsg=cmdmsg)
  if (cmdstat /= 0) error stop 4
  if (trim(cmdmsg) /= 'unchanged') error stop 5
  call wait_for_async_file()

  print '(a)', 'ok'
contains
  subroutine wait_for_async_file()
    character(len=16) :: line
    integer :: clock_rate, current_count, io_status, start_count, unit
    logical :: exists

    call system_clock(count=start_count, count_rate=clock_rate)
    if (clock_rate <= 0) error stop 6
    do
      inquire(file='ar53_shell_async.txt', exist=exists)
      if (exists) then
        line = ''
        open(newunit=unit, file='ar53_shell_async.txt', status='old', &
             action='read', iostat=io_status)
        if (io_status == 0) then
          read(unit, '(a)', iostat=io_status) line
          close(unit)
          if (io_status == 0 .and. trim(line) == 'async') return
        end if
      end if
      call system_clock(count=current_count)
      if (current_count - start_count >= 5 * clock_rate) error stop 7
    end do
  end subroutine wait_for_async_file
end program ar53_execute_command_line_path
