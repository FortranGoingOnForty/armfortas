! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! FILE_MISSING: mixed-defined-readonly.tmp
module mixed_defined_io_items_support
  implicit none

  type :: box
    integer :: value = -1
  end type box

  integer :: formatted_write_calls = 0
  integer :: formatted_read_calls = 0
  integer :: unformatted_write_calls = 0
  integer :: unformatted_read_calls = 0
  logical :: fail_formatted_write = .false.
  logical :: fail_formatted_read = .false.

  interface write(formatted)
    module procedure write_box_formatted
  end interface

  interface read(formatted)
    module procedure read_box_formatted
  end interface

  interface write(unformatted)
    module procedure write_box_unformatted
  end interface

  interface read(unformatted)
    module procedure read_box_unformatted
  end interface

contains

  subroutine write_box_formatted(value, unit, iotype, v_list, iostat, iomsg)
    class(box), intent(in) :: value
    integer, intent(in) :: unit
    character(len=*), intent(in) :: iotype
    integer, intent(in) :: v_list(:)
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    formatted_write_calls = formatted_write_calls + 1
    if (fail_formatted_write) then
      iostat = 91
      iomsg = 'mixed defined write failed'
      return
    end if
    if (iotype /= 'LISTDIRECTED' .or. size(v_list) /= 0) error stop 10
    write(unit, '(A,I0,A)', advance='no', iostat=iostat, iomsg=iomsg) &
      '<', value%value, '>'
  end subroutine write_box_formatted

  subroutine read_box_formatted(value, unit, iotype, v_list, iostat, iomsg)
    class(box), intent(inout) :: value
    integer, intent(in) :: unit
    character(len=*), intent(in) :: iotype
    integer, intent(in) :: v_list(:)
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    formatted_read_calls = formatted_read_calls + 1
    if (fail_formatted_read) then
      iostat = 92
      iomsg = 'mixed defined read failed'
      return
    end if
    if (iotype /= 'LISTDIRECTED' .or. size(v_list) /= 0) error stop 11
    read(unit, *, iostat=iostat, iomsg=iomsg) value%value
  end subroutine read_box_formatted

  subroutine write_box_unformatted(value, unit, iostat, iomsg)
    class(box), intent(in) :: value
    integer, intent(in) :: unit
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    unformatted_write_calls = unformatted_write_calls + 1
    write(unit, iostat=iostat, iomsg=iomsg) value%value
  end subroutine write_box_unformatted

  subroutine read_box_unformatted(value, unit, iostat, iomsg)
    class(box), intent(inout) :: value
    integer, intent(in) :: unit
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    unformatted_read_calls = unformatted_read_calls + 1
    read(unit, iostat=iostat, iomsg=iomsg) value%value
  end subroutine read_box_unformatted

end module mixed_defined_io_items_support

program mixed_defined_io_items
  use mixed_defined_io_items_support
  implicit none

  type(box) :: first, second
  integer :: unit, ios, prefix, suffix
  integer :: prefix_pos, value_pos, suffix_pos
  character(len=128) :: line, message

  first%value = 7
  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, *, iostat=ios) 'prefix', first, 'suffix'
  if (ios /= 0 .or. formatted_write_calls /= 1) error stop 1
  rewind(unit)
  line = ''
  read(unit, '(A)', iostat=ios) line
  prefix_pos = index(line, 'prefix')
  value_pos = index(line, '<7>')
  suffix_pos = index(line, 'suffix')
  if (ios /= 0 .or. prefix_pos == 0 .or. value_pos <= prefix_pos .or. &
      suffix_pos <= value_pos) error stop 2
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(I0,1X,I0,1X,I0)') 11, 22, 33
  rewind(unit)
  first%value = -1
  prefix = -1
  suffix = -1
  read(unit, *, iostat=ios) prefix, first, suffix
  if (ios /= 0 .or. formatted_read_calls /= 1) error stop 3
  if (prefix /= 11 .or. first%value /= 22 .or. suffix /= 33) error stop 4
  close(unit)

  first%value = 7
  second%value = 8
  formatted_write_calls = 0
  fail_formatted_write = .true.
  message = 'sentinel'
  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, *, iostat=ios, iomsg=message) 'prefix', first, second
  if (ios /= 91 .or. formatted_write_calls /= 1) error stop 5
  if (trim(message) /= 'mixed defined write failed') error stop 6
  close(unit)
  fail_formatted_write = .false.

  formatted_write_calls = 0
  open(newunit=unit, file='mixed-defined-readonly.tmp', status='replace', &
       action='write', form='formatted')
  write(unit, '(A)') 'seed'
  close(unit)
  open(newunit=unit, file='mixed-defined-readonly.tmp', status='old', &
       action='read', form='formatted')
  message = 'sentinel'
  write(unit, *, iostat=ios, iomsg=message) 'prefix', first
  if (ios == 0 .or. formatted_write_calls /= 0) error stop 15
  if (index(message, 'unit not open for writing') == 0) error stop 16
  close(unit, status='delete')

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(I0,1X,I0,1X,I0)') 11, 22, 33
  rewind(unit)
  formatted_read_calls = 0
  fail_formatted_read = .true.
  prefix = -1
  suffix = -1
  message = 'sentinel'
  read(unit, *, iostat=ios, iomsg=message) prefix, first, suffix
  if (ios /= 92 .or. formatted_read_calls /= 1) error stop 7
  if (prefix /= 11 .or. suffix /= -1) error stop 8
  if (trim(message) /= 'mixed defined read failed') error stop 9
  close(unit)
  fail_formatted_read = .false.

  first%value = 22
  open(newunit=unit, status='scratch', action='readwrite', form='unformatted')
  write(unit, iostat=ios) 11, first, 33
  if (ios /= 0 .or. unformatted_write_calls /= 1) error stop 12
  rewind(unit)
  prefix = -1
  first%value = -1
  suffix = -1
  read(unit, iostat=ios) prefix, first, suffix
  if (ios /= 0 .or. unformatted_read_calls /= 1) error stop 13
  if (prefix /= 11 .or. first%value /= 22 .or. suffix /= 33) error stop 14
  close(unit)

  print *, 'ok'
end program mixed_defined_io_items
