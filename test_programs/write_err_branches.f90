! CHECK: ok
module write_err_branches_support
  implicit none

  type :: box
    integer :: value
  end type box

  integer :: defined_write_calls

  interface write(formatted)
    module procedure write_box
  end interface

contains

  subroutine write_box(value, unit, iotype, v_list, iostat, iomsg)
    class(box), intent(in) :: value
    integer, intent(in) :: unit
    character(len=*), intent(in) :: iotype
    integer, intent(in) :: v_list(:)
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    defined_write_calls = defined_write_calls + 1
    iostat = 91
    iomsg = 'defined write failed'
  end subroutine write_box

end module write_err_branches_support

program write_err_branches
  use write_err_branches_support
  implicit none

  character(len=*), parameter :: path = 'write_err_branches.tmp'
  character(len=3) :: buffer
  character(len=32) :: line, message
  integer :: unit, read_status, value
  type(box) :: object, second_object
  namelist /group/ value

  open(newunit=unit, file=path, status='replace', action='write')
  write(unit, '(A)') 'kept'
  close(unit)

  message = 'sentinel'
  open(newunit=unit, file=path, status='old', action='read')
  write(unit, *, err=100, iomsg=message) 'discarded'
  error stop 1
100 continue
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 2
  close(unit)

  open(newunit=unit, file=path, status='old', action='read')
  write(unit, '(A)', err=200) 'discarded'
  error stop 3
200 continue
  close(unit)

  buffer = 'abc'
  write(buffer, *, err=300) 'discarded'
  error stop 4
300 continue
  if (buffer /= 'abc') error stop 5

  buffer = 'abc'
  write(buffer, '(A)', err=400) 'discarded'
  error stop 6
400 continue
  if (buffer /= 'abc') error stop 7

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') 'position'
  write(unit, *, pos=0, err=500) 'discarded'
  error stop 8
500 continue
  rewind(unit)
  line = ''
  read(unit, '(A)', iostat=read_status) line
  if (read_status /= 0 .or. trim(line) /= 'position') error stop 9
  read(unit, '(A)', iostat=read_status) line
  if (read_status >= 0) error stop 10
  close(unit)

  value = 42
  open(newunit=unit, file=path, status='old', action='read')
  write(unit, nml=group, err=600)
  error stop 11
600 continue
  close(unit)

  object%value = 7
  defined_write_calls = 0
  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, *, err=700) object
  error stop 12
700 continue
  if (defined_write_calls /= 1) error stop 13
  close(unit)

  second_object%value = 8
  defined_write_calls = 0
  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, *, err=800) object, second_object
  error stop 14
800 continue
  if (defined_write_calls /= 1) error stop 15
  close(unit)

  open(newunit=unit, file=path, status='old', action='read')
  line = ''
  read(unit, '(A)', iostat=read_status) line
  if (read_status /= 0 .or. trim(line) /= 'kept') error stop 16
  read(unit, '(A)', iostat=read_status) line
  if (read_status >= 0) error stop 17
  close(unit, status='delete')

  print *, 'ok'
end program write_err_branches
