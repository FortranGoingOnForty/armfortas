! CHECK: ok
! IR_CHECK: call @afs_backspace_ex(
! IR_CHECK: call @afs_endfile_ex(
program backspace_endfile_statements
  use, intrinsic :: iso_fortran_env, only : int8, int16, int32, int64, iostat_end
  implicit none

  character(len=*), parameter :: path = 'backspace_endfile.tmp'
  character(len=*), parameter :: readonly_path = 'endfile_readonly.tmp'
  character(len=16) :: text
  character(len=64) :: message
  integer :: unit, ios, value
  integer(int8) :: status1(5)
  integer(int16) :: status2(4)
  integer(int32) :: status4(3)
  integer(int64) :: status8(3)

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

  status1 = [11_int8, 99_int8, 22_int8, 33_int8, 44_int8]
  backspace(unit=997, iostat=status1(2))
  if (any(status1 /= [11_int8, 1_int8, 22_int8, 33_int8, 44_int8])) error stop 18

  status2 = [111_int16, 999_int16, 222_int16, 333_int16]
  endfile(unit=996, iostat=status2(2))
  if (any(status2 /= [111_int16, 1_int16, 222_int16, 333_int16])) error stop 19

  status4 = [1111_int32, 9999_int32, 2222_int32]
  backspace(unit=995, iostat=status4(2))
  if (any(status4 /= [1111_int32, 1_int32, 2222_int32])) error stop 20

  status8 = [11111_int64, 99999_int64, 22222_int64]
  endfile(unit=994, iostat=status8(2))
  if (any(status8 /= [11111_int64, 1_int64, 22222_int64])) error stop 21

  print *, 'ok'
end program backspace_endfile_statements
