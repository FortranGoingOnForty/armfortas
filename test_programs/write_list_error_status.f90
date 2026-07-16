! CHECK: ok
program write_list_error_status
  implicit none

  character(len=*), parameter :: path = 'write_list_error_status.tmp'
  character(len=64) :: message
  integer :: unit, ios

  open(newunit=unit, file=path, status='replace', action='write')
  write(unit, *) 1
  close(unit)

  open(newunit=unit, file=path, status='old', action='read')
  ios = 77
  message = 'sentinel'
  write(unit, *, iostat=ios, iomsg=message) 42
  if (ios == 0) error stop 1
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 2
  close(unit, status='delete')

  print *, 'ok'
end program write_list_error_status
