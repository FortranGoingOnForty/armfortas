! CHECK: ok
! IR_CHECK: call @afs_backspace_ex(
! IR_CHECK: call @afs_endfile_ex(
program backspace_endfile_statements
  use, intrinsic :: iso_fortran_env, only : iostat_end
  implicit none

  character(len=*), parameter :: path = 'backspace_endfile.tmp'
  character(len=*), parameter :: readonly_path = 'endfile_readonly.tmp'
  character(len=16) :: text
  character(len=64) :: message
  integer :: unit, ios, value

  open(newunit=unit, file=path, status='replace', action='write')
  write(unit, '(A)') 'first'
  write(unit, '(A)') 'second'
  close(unit)

  open(newunit=unit, file=path, status='old', action='readwrite')
  read(unit, '(A)') text
  ios = 77
  message = 'sentinel'
  backspace(unit=unit, iostat=ios, iomsg=message)
  if (ios /= 0) error stop 1
  read(unit, '(A)') text
  if (trim(text) /= 'first') error stop 2

  ios = 88
  message = 'sentinel'
  endfile(unit=unit, iostat=ios, iomsg=message)
  if (ios /= 0) error stop 3
  close(unit)

  open(newunit=unit, file=path, status='old', action='read')
  read(unit, '(A)', iostat=ios) text
  if (ios /= 0 .or. trim(text) /= 'first') error stop 4
  read(unit, '(A)', iostat=ios) text
  if (ios /= iostat_end) error stop 5
  close(unit, status='delete')

  open(newunit=unit, status='scratch', form='unformatted', action='readwrite')
  write(unit) 11
  write(unit) 22
  rewind(unit)
  read(unit) value
  backspace(unit=unit, iostat=ios)
  if (ios /= 0) error stop 6
  read(unit) value
  if (value /= 11) error stop 7
  endfile(unit=unit, iostat=ios)
  if (ios /= 0) error stop 8
  rewind(unit)
  read(unit, iostat=ios) value
  if (ios /= 0 .or. value /= 11) error stop 9
  read(unit, iostat=ios) value
  if (ios /= iostat_end) error stop 10
  close(unit)

  ios = 99
  message = 'sentinel'
  backspace(unit=999, iostat=ios, iomsg=message, err=100)
  error stop 11
100 continue
  if (ios == 0) error stop 12
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 13

  backspace(998, err=150)
  error stop 14
150 continue

  open(newunit=unit, file=readonly_path, status='replace', action='write')
  write(unit, '(A)') 'keep'
  close(unit)
  open(newunit=unit, file=readonly_path, status='old', action='read')
  ios = 111
  message = 'sentinel'
  endfile(unit=unit, iostat=ios, iomsg=message, err=200)
  error stop 15
200 continue
  if (ios == 0) error stop 16
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 17
  close(unit, status='delete')

  print *, 'ok'
end program backspace_endfile_statements
