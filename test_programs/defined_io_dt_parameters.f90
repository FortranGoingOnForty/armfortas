! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module defined_io_dt_parameters_support
  implicit none

  type :: box
    integer :: value = -1
  end type box

  integer :: write_calls = 0
  integer :: read_calls = 0

  interface write(formatted)
    module procedure write_box_formatted
  end interface

  interface read(formatted)
    module procedure read_box_formatted
  end interface

contains

  subroutine write_box_formatted(value, unit, iotype, v_list, iostat, iomsg)
    class(box), intent(in) :: value
    integer, intent(in) :: unit
    character(len=*), intent(in) :: iotype
    integer, intent(in) :: v_list(:)
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    write_calls = write_calls + 1
    select case (write_calls)
    case (1)
      if (iotype /= 'DTFirst Tag' .or. size(v_list) /= 3) then
        iostat = 101
        iomsg = 'first DT metadata mismatch'
        return
      end if
      if (v_list(1) /= 7 .or. v_list(2) /= -2 .or. v_list(3) /= 3) then
        iostat = 102
        iomsg = 'first DT value list mismatch'
        return
      end if
    case (2)
      if (iotype /= 'DTSECOND' .or. size(v_list) /= 2) then
        iostat = 103
        iomsg = 'second DT metadata mismatch'
        return
      end if
      if (v_list(1) /= 0 .or. v_list(2) /= -9) then
        iostat = 104
        iomsg = 'second DT value list mismatch'
        return
      end if
    case default
      iostat = 105
      iomsg = 'unexpected defined WRITE call'
      return
    end select

    write(unit, '(I0,1X)', advance='no', iostat=iostat, iomsg=iomsg) value%value
  end subroutine write_box_formatted

  subroutine read_box_formatted(value, unit, iotype, v_list, iostat, iomsg)
    class(box), intent(inout) :: value
    integer, intent(in) :: unit
    character(len=*), intent(in) :: iotype
    integer, intent(in) :: v_list(:)
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    read_calls = read_calls + 1
    select case (read_calls)
    case (1)
      if (iotype /= 'DTleft' .or. size(v_list) /= 1) then
        iostat = 111
        iomsg = 'first READ metadata mismatch'
        return
      end if
      if (v_list(1) /= -4) then
        iostat = 112
        iomsg = 'first READ value list mismatch'
        return
      end if
    case (2)
      if (iotype /= 'DTRight Tag' .or. size(v_list) /= 3) then
        iostat = 113
        iomsg = 'second READ metadata mismatch'
        return
      end if
      if (v_list(1) /= 5 .or. v_list(2) /= 0 .or. v_list(3) /= -6) then
        iostat = 114
        iomsg = 'second READ value list mismatch'
        return
      end if
    case default
      iostat = 115
      iomsg = 'unexpected defined READ call'
      return
    end select

    read(unit, *, iostat=iostat, iomsg=iomsg) value%value
  end subroutine read_box_formatted

end module defined_io_dt_parameters_support

program defined_io_dt_parameters
  use defined_io_dt_parameters_support
  implicit none

  type(box) :: first, second
  integer :: unit, ios
  character(len=128) :: message
  character(len=*), parameter :: output_format = &
    '(DT"First Tag"(7,-2,+3),DT''SECOND''(0,-9))'

  first%value = 11
  second%value = 22
  message = 'sentinel'
  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, fmt=output_format, iostat=ios, iomsg=message) first, second
  if (ios /= 0 .or. write_calls /= 2) error stop 1
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(I0,1X,I0)') 31, 42
  rewind(unit)
  first%value = -1
  second%value = -1
  message = 'sentinel'
  read(unit, 100, iostat=ios, iomsg=message) first, second
  if (ios /= 0 .or. read_calls /= 2) error stop 2
  if (first%value /= 31 .or. second%value /= 42) error stop 3
  close(unit)

  print *, 'ok'
100 format(DT'left'(-4), DT"Right Tag"(+5,0,-6))
end program defined_io_dt_parameters
