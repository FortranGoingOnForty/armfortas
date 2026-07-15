! CHECK: ok
program read_first_error_iomsg
  implicit none

  character(len=16) :: source
  character(len=64) :: message
  integer :: first, second, values(2)
  integer :: unit, ios

  source = 'bad 42'
  first = 7
  second = 8
  ios = 77
  message = 'sentinel'
  read(source, *, iostat=ios, iomsg=message) first, second
  if (ios == 0 .or. first /= 7 .or. second /= 8) error stop 1
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 2

  source = 'xx42'
  first = 7
  second = 8
  ios = 77
  message = 'sentinel'
  read(source, '(I2,I2)', iostat=ios, iomsg=message) first, second
  if (ios == 0 .or. first /= 7 .or. second /= 8) error stop 3
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 4

  source = 'bad 42'
  values = [7, 8]
  ios = 77
  message = 'sentinel'
  read(source, *, iostat=ios, iomsg=message) values
  if (ios == 0 .or. any(values /= [7, 8])) error stop 5
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 6

  open(newunit=unit, status='scratch', action='readwrite')
  write(unit, '(A)') 'bad 42'
  rewind(unit)
  first = 7
  second = 8
  ios = 77
  message = 'sentinel'
  read(unit, *, iostat=ios, iomsg=message) first, second
  if (ios == 0 .or. first /= 7 .or. second /= 8) error stop 7
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 8
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite')
  write(unit, '(A)') 'xx42'
  rewind(unit)
  first = 7
  second = 8
  ios = 77
  message = 'sentinel'
  read(unit, '(I2,I2)', iostat=ios, iomsg=message) first, second
  if (ios == 0 .or. first /= 7 .or. second /= 8) error stop 9
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 10
  close(unit)

  open(newunit=unit, status='scratch', access='stream', form='formatted', &
       action='readwrite')
  write(unit, '(A)') '42'
  first = 7
  ios = 77
  message = 'sentinel'
  read(unit, *, pos=0, iostat=ios, iomsg=message) first
  if (ios == 0 .or. first /= 7) error stop 11
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 12
  close(unit)

  print *, 'ok'
end program read_first_error_iomsg
