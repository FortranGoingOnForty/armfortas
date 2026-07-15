! CHECK: ok
program read_namelist_io_error
  implicit none

  character(len=64) :: message
  integer :: unit, ios, value
  namelist /wanted/ value

  open(newunit=unit, file='.', status='old', action='read', iostat=ios)
  if (ios /= 0) error stop 1

  value = 17
  ios = 0
  message = 'sentinel'
  read(unit, nml=wanted, iostat=ios, iomsg=message, end=900, err=100)
  error stop 2

100 continue
  close(unit)
  if (ios <= 0) error stop 3
  if (value /= 17) error stop 4
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 5
  print *, 'ok'
  stop

900 error stop 6
end program read_namelist_io_error
