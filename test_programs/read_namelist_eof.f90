! CHECK: ok
program read_namelist_eof
  use, intrinsic :: iso_fortran_env, only : iostat_end
  implicit none

  character(len=*), parameter :: empty_path = 'read_namelist_empty.tmp'
  character(len=*), parameter :: missing_path = 'read_namelist_missing.tmp'
  character(len=*), parameter :: incomplete_path = 'read_namelist_incomplete.tmp'
  character(len=16) :: internal_record
  integer :: unit, ios, value
  namelist /wanted/ value

  open(newunit=unit, file=empty_path, status='replace', action='write')
  close(unit)
  open(newunit=unit, file=empty_path, status='old', action='read')
  value = 7
  ios = 77
  read(unit, nml=wanted, iostat=ios)
  close(unit, status='delete')
  if (ios /= iostat_end .or. value /= 7) error stop 1

  open(newunit=unit, file=missing_path, status='replace', action='write')
  write(unit, '(A)') '&other value=42 /'
  close(unit)
  open(newunit=unit, file=missing_path, status='old', action='read')
  value = 8
  ios = 77
  read(unit, nml=wanted, iostat=ios)
  close(unit, status='delete')
  if (ios /= iostat_end .or. value /= 8) error stop 2

  open(newunit=unit, file=incomplete_path, status='replace', action='write')
  write(unit, '(A)') '&wanted value=42'
  close(unit)
  open(newunit=unit, file=incomplete_path, status='old', action='read')
  value = 9
  ios = 77
  read(unit, nml=wanted, iostat=ios)
  close(unit, status='delete')
  if (ios /= iostat_end .or. value /= 42) error stop 3

  internal_record = ''
  value = 10
  ios = 77
  read(internal_record, nml=wanted, iostat=ios)
  if (ios /= iostat_end .or. value /= 10) error stop 4

  print *, 'ok'
end program read_namelist_eof
