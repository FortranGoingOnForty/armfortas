! CHECK: ok
program read_namelist_eof
  use, intrinsic :: iso_fortran_env, only : iostat_end
  implicit none

  character(len=*), parameter :: empty_path = 'read_namelist_empty.tmp'
  character(len=*), parameter :: missing_path = 'read_namelist_missing.tmp'
  character(len=*), parameter :: incomplete_path = 'read_namelist_incomplete.tmp'
  character(len=*), parameter :: position_path = 'read_namelist_position.tmp'
  character(len=16) :: internal_record
  character(len=64) :: message
  integer :: unit, ios, value
  namelist /wanted/ value

  open(newunit=unit, file=empty_path, status='replace', action='write')
  close(unit)
  open(newunit=unit, file=empty_path, status='old', action='read')
  value = 7
  ios = 77
  message = 'sentinel'
  read(unit, nml=wanted, iostat=ios, iomsg=message)
  close(unit, status='delete')
  if (ios /= iostat_end .or. value /= 7) error stop 1
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 5

  open(newunit=unit, file=missing_path, status='replace', action='write')
  write(unit, '(A)') '&other value=42 /'
  close(unit)
  open(newunit=unit, file=missing_path, status='old', action='read')
  value = 8
  ios = 77
  message = 'sentinel'
  read(unit, nml=wanted, iostat=ios, iomsg=message)
  close(unit, status='delete')
  if (ios /= iostat_end .or. value /= 8) error stop 2
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 6

  open(newunit=unit, file=incomplete_path, status='replace', action='write')
  write(unit, '(A)') '&wanted value=42'
  close(unit)
  open(newunit=unit, file=incomplete_path, status='old', action='read')
  value = 9
  ios = 77
  message = 'sentinel'
  read(unit, nml=wanted, iostat=ios, iomsg=message)
  close(unit, status='delete')
  if (ios /= iostat_end .or. value /= 42) error stop 3
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 7

  internal_record = ''
  value = 10
  ios = 77
  message = 'sentinel'
  read(internal_record, nml=wanted, iostat=ios, iomsg=message)
  if (ios /= iostat_end .or. value /= 10) error stop 4
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 8

  open(newunit=unit, file=position_path, status='replace', action='write')
  write(unit, '(A)') '&wanted value=42 /'
  close(unit)
  open(newunit=unit, file=position_path, status='old', access='stream', &
       form='formatted', action='read')
  value = 11
  ios = 77
  message = 'sentinel'
  read(unit, nml=wanted, pos=0, iostat=ios, iomsg=message)
  close(unit, status='delete')
  if (ios == 0 .or. ios == iostat_end .or. value /= 11) error stop 9
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 10

  print *, 'ok'
end program read_namelist_eof
