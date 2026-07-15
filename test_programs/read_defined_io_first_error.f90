! CHECK: ok
module read_defined_io_first_error_support
  implicit none

  type :: box
    integer :: value
  end type box

  integer :: read_calls

  interface read(formatted)
    module procedure read_box
  end interface

contains

  subroutine read_box(value, unit, iotype, v_list, iostat, iomsg)
    class(box), intent(inout) :: value
    integer, intent(in) :: unit
    character(len=*), intent(in) :: iotype
    integer, intent(in) :: v_list(:)
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    read_calls = read_calls + 1
    if (read_calls == 1) then
      iostat = 91
      iomsg = 'first defined read failed'
    else
      value%value = 999
      iostat = 0
      iomsg = ''
    end if
  end subroutine read_box

end module read_defined_io_first_error_support

program read_defined_io_first_error
  use read_defined_io_first_error_support
  implicit none

  type(box) :: first, second
  integer :: unit, ios
  character(len=64) :: message

  open(newunit=unit, status='scratch', action='readwrite')
  write(unit, '(A)') 'ignored'
  rewind(unit)

  first%value = 7
  second%value = 8
  read_calls = 0
  ios = 77
  message = 'sentinel'
  read(unit, *, iostat=ios, iomsg=message) first, second

  if (ios /= 91 .or. read_calls /= 1) error stop 1
  if (first%value /= 7 .or. second%value /= 8) error stop 2
  if (trim(message) /= 'first defined read failed') error stop 3
  close(unit)

  open(newunit=unit, status='scratch', access='stream', form='formatted', action='readwrite')
  write(unit, '(A)') 'ignored'
  first%value = 7
  read_calls = 0
  ios = 77
  message = 'sentinel'
  read(unit, *, pos=0, iostat=ios, iomsg=message) first
  if (ios == 0 .or. read_calls /= 0) error stop 4
  if (first%value /= 7) error stop 5
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 6
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite')
  write(unit, '(A)') 'ignored'
  rewind(unit)
  first%value = 7
  read_calls = 0
  ios = 77
  read(unit, *, iostat=ios) first
  if (ios /= 91 .or. read_calls /= 1) error stop 7
  if (first%value /= 7) error stop 8
  close(unit)

  print *, 'ok'
end program read_defined_io_first_error
