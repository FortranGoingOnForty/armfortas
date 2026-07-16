! CHECK: ok
program list_read_blank_records
  use, intrinsic :: iso_fortran_env, only : iostat_end
  implicit none

  character(len=*), parameter :: path = 'list_read_blank_records.tmp'
  character(len=64) :: message
  integer :: unit, ios, first, second

  open(newunit=unit, file=path, status='replace', action='write')
  write(unit, '(A)') ''
  write(unit, '(A)') '   '
  write(unit, '(A)') '42'
  write(unit, '(A)') ''
  write(unit, '(A)') '17'
  close(unit)

  open(newunit=unit, file=path, status='old', action='read')
  first = -1
  second = -2
  ios = 77
  message = 'sentinel'
  read(unit, *, iostat=ios, iomsg=message) first, second
  if (ios /= 0) error stop 1
  if (first /= 42 .or. second /= 17) error stop 2

  ios = 88
  message = 'sentinel'
  read(unit, *, iostat=ios, iomsg=message, end=100) first
  error stop 3
100 continue
  if (ios /= iostat_end) error stop 4
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 5
  close(unit, status='delete')

  print *, 'ok'
end program list_read_blank_records
