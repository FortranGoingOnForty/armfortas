! CHECK: ok
! IR_CHECK: call @afs_lst_begin_internal_fixed(
! IR_CHECK: call @afs_write_internal_string(
! IR_CHECK: call @afs_lst_end_internal_fixed(
program write_internal_record_overflow
  use, intrinsic :: iso_fortran_env, only : iostat_eor
  implicit none

  character(len=3) :: buffer
  character(len=3) :: records(2)
  character(len=64) :: message
  integer :: ios

  buffer = '???'
  message = 'sentinel'
  ios = 77
  write(buffer, '(A)', iostat=ios, iomsg=message) 'abcdef'
  if (ios /= iostat_eor) error stop 1
  if (buffer /= '???') error stop 2
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 3

  records = ['aaa', 'bbb']
  message = 'sentinel'
  ios = 77
  write(records, '(A)', iostat=ios, iomsg=message) 'abcdef'
  if (ios /= iostat_eor) error stop 4
  if (any(records /= ['aaa', 'bbb'])) error stop 5
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 6

  buffer = '???'
  message = 'sentinel'
  ios = 77
  write(buffer, *, iostat=ios, iomsg=message) 'abcdef'
  if (ios /= iostat_eor) error stop 7
  if (buffer /= '???') error stop 8
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 9

  records = ['aaa', 'bbb']
  message = 'sentinel'
  ios = 77
  write(records, *, iostat=ios, iomsg=message) 'abcdef'
  if (ios /= iostat_eor) error stop 10
  if (any(records /= ['aaa', 'bbb'])) error stop 11
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 12

  buffer = 'xyz'
  message = 'sentinel'
  ios = 77
  write(buffer(2:3), *, iostat=ios, iomsg=message) 'abc'
  if (ios /= iostat_eor) error stop 13
  if (buffer /= 'xyz') error stop 14
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 15

  buffer = '???'
  message = 'sentinel'
  ios = 77
  write(buffer, *, iostat=ios, iomsg=message) 'a'
  if (ios /= 0) error stop 16
  if (buffer /= ' a ') error stop 17
  if (len_trim(message) /= 0) error stop 18

  print *, 'ok'
end program write_internal_record_overflow
