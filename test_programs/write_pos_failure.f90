! CHECK: ok
program write_pos_failure
  implicit none

  integer :: unit, ios, value
  integer(1) :: narrow_status(2)
  character(len=32) :: message
  namelist /group/ value

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') 'list'
  narrow_status = [77_1, 22_1]
  message = 'sentinel'
  write(unit, *, pos=0, iostat=narrow_status(1), iomsg=message) 'discarded'
  if (narrow_status(1) == 0 .or. narrow_status(2) /= 22) error stop 1
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 2
  call require_single_record(unit, 'list', 3)
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') 'formatted'
  ios = 77
  message = 'sentinel'
  write(unit, '(A)', pos=0, iostat=ios, iomsg=message) 'discarded'
  if (ios == 0) error stop 4
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 5
  call require_single_record(unit, 'formatted', 6)
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') 'namelist'
  value = 42
  ios = 77
  write(unit, nml=group, pos=0, iostat=ios)
  if (ios == 0) error stop 7
  call require_single_record(unit, 'namelist', 8)
  close(unit)

  open(newunit=unit, status='scratch', access='stream', form='unformatted', &
       action='readwrite')
  value = 12345
  ios = 77
  write(unit, pos=1, iostat=ios) value
  if (ios /= 0) error stop 9
  value = 0
  read(unit, pos=1, iostat=ios) value
  if (ios /= 0 .or. value /= 12345) error stop 10
  close(unit)

  print *, 'ok'

contains

  subroutine require_single_record(unit, expected, code)
    integer, intent(in) :: unit, code
    character(len=*), intent(in) :: expected
    character(len=32) :: actual
    integer :: read_status

    rewind(unit)
    actual = ''
    read(unit, '(A)', iostat=read_status) actual
    if (read_status /= 0 .or. trim(actual) /= expected) error stop code
    read(unit, '(A)', iostat=read_status) actual
    if (read_status >= 0) error stop code
  end subroutine require_single_record

end program write_pos_failure
