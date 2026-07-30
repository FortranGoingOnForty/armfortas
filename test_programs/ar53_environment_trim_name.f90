! GET_ENVIRONMENT_VARIABLE must use NAME exactly when TRIM_NAME is false.
! A padded CHARACTER variable therefore names a different variable than its
! trimmed prefix; omitted or true TRIM_NAME must retain the default trimming.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_get_environment_variable_trim(
program ar53_environment_trim_name
  implicit none

  character(len=64) :: name, value
  integer :: length, status
  logical :: preserve_blanks

  name = 'PATH'
  preserve_blanks = .false.

  value = '?'
  length = -1
  status = -1
  call get_environment_variable(name, value, length, status)
  if (status > 0 .or. length <= 0 .or. len_trim(value) <= 0) error stop 1

  value = '?'
  length = -1
  status = -1
  call get_environment_variable(name, value, length, status, .true.)
  if (status > 0 .or. length <= 0 .or. len_trim(value) <= 0) error stop 2

  value = '?'
  length = -1
  status = -1
  call get_environment_variable(name, value, length, status, .false.)
  if (status <= 0) error stop 3
  if (length /= 0) error stop 4
  if (len_trim(value) /= 0) error stop 5

  value = '?'
  length = -1
  status = -1
  call get_environment_variable(name, value=value, length=length, status=status, &
                                trim_name=preserve_blanks)
  if (status <= 0) error stop 6
  if (length /= 0) error stop 7
  if (len_trim(value) /= 0) error stop 8

  print '(a)', 'ok'
end program ar53_environment_trim_name
