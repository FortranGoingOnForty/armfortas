! CHECK: ok
! IR_CHECK: call @afs_lst_begin_internal_fixed(
! IR_CHECK: call @afs_write_internal_string(
! IR_CHECK: call @afs_lst_end_internal_fixed(
program write_internal_record_overflow
  use, intrinsic :: iso_fortran_env, only : iostat_eor
  implicit none

  character(len=3) :: buffer
  character(len=3) :: records(2)
  character(len=0) :: empty
  character(len=3), allocatable :: zero_records(:), missing_records(:)
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

  message = 'sentinel'
  ios = 77
  write(empty, *, iostat=ios, iomsg=message) 'a'
  if (ios /= iostat_eor) error stop 19
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 20

  allocate(zero_records(0))
  message = 'sentinel'
  ios = 77
  write(zero_records, *, iostat=ios, iomsg=message) 'a'
  if (ios == 0) error stop 21
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 22
  if (.not. allocated(zero_records) .or. size(zero_records) /= 0) error stop 23

  message = 'sentinel'
  ios = 77
  write(missing_records, *, iostat=ios, iomsg=message) 'a'
  if (ios == 0) error stop 24
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 25
  if (allocated(missing_records)) error stop 26

  print *, 'ok'
end program write_internal_record_overflow
