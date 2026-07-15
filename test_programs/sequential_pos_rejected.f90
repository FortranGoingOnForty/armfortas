! CHECK: ok
program sequential_pos_rejected
  implicit none

  integer :: unit, ios
  character(len=32) :: message, actual

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') 'sequential write'
  ios = 77
  message = 'sentinel'
  write(unit, '(A)', pos=1, iostat=ios, iomsg=message) 'discarded'
  if (ios == 0) error stop 1
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 2
  call require_single_record(unit, 'sequential write', 3)
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') 'sequential read'
  ios = 77
  message = 'sentinel'
  actual = 'unchanged'
  read(unit, '(A)', pos=1, iostat=ios, iomsg=message) actual
  if (ios == 0) error stop 4
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 5
  if (trim(actual) /= 'unchanged') error stop 6
  call require_single_record(unit, 'sequential read', 7)
  close(unit)

  print *, 'ok'

contains

  subroutine require_single_record(unit, expected, code)
    integer, intent(in) :: unit, code
    character(len=*), intent(in) :: expected
    character(len=32) :: record
    integer :: read_status

    rewind(unit)
    record = ''
    read(unit, '(A)', iostat=read_status) record
    if (read_status /= 0 .or. trim(record) /= expected) error stop code
    read(unit, '(A)', iostat=read_status) record
    if (read_status >= 0) error stop code
  end subroutine require_single_record

end program sequential_pos_rejected
