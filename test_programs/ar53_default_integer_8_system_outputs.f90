! System intrinsic output arguments use the active default INTEGER kind.
! The runtime returns i32 status/length values, so default-INTEGER(8)
! destinations require an i32 temporary followed by a full-width writeback.
!
! FLAGS: -fdefault-integer-8
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: alloca i32
! IR_CHECK: call @afs_get_command_argument(
! IR_CHECK: int_extend
! IR_CHECK: call @afs_get_command(
! IR_CHECK: call @afs_get_environment_variable_trim(
! IR_CHECK: call @afs_execute_command_line(
program ar53_default_integer_8_system_outputs
  implicit none

  integer, parameter :: poison = 1234605616436508552_8
  type :: result_type
    integer :: length
    integer :: status
  end type result_type

  type(result_type) :: argument
  integer :: command_result(2)
  integer :: environment_length, environment_status
  integer :: exit_status, command_status
  character(len=1) :: text

  argument%length = poison
  argument%status = poison
  call get_command_argument(0, text, argument%length, argument%status)
  if (argument%length <= 1 .or. argument%length > 4096) error stop 1
  if (argument%status /= -1) error stop 2

  command_result = poison
  call get_command(text, command_result(1), command_result(2))
  if (command_result(1) <= 1 .or. command_result(1) > 4096) error stop 3
  if (command_result(2) /= -1) error stop 4

  environment_length = poison
  environment_status = poison
  call get_environment_variable( &
    'AFS_AR53_R017_MISSING_8F2C913D', &
    length=environment_length, status=environment_status)
  if (environment_length /= 0 .or. environment_status /= 1) error stop 5

  exit_status = poison
  command_status = poison
  call execute_command_line('exit 7', exitstat=exit_status, cmdstat=command_status)
  if (exit_status /= 7 .or. command_status /= 0) error stop 6

  print '(a)', 'ok'
end program ar53_default_integer_8_system_outputs
