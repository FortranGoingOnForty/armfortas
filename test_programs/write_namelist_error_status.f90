! CHECK: ok
program write_namelist_error_status
  implicit none

  integer :: unit, ios, value
  character(len=64) :: message
  namelist /group/ value

  value = 42
  open(newunit=unit, status='scratch', action='read')
  ios = 77
  message = 'sentinel'
  write(unit, nml=group, iostat=ios, iomsg=message)
  if (ios == 0) error stop 1
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 2
  close(unit)

  print *, 'ok'
end program write_namelist_error_status
