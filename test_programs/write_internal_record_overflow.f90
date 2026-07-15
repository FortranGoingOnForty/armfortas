! CHECK: ok
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

  print *, 'ok'
end program write_internal_record_overflow
