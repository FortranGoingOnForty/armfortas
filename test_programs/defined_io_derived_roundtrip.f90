! CHECK: ok
! IR_CHECK: write_box_formatted
! IR_CHECK: read_box_unformatted
! REPRO_CHECK: run
module defined_io_derived_roundtrip_mod
  implicit none

  type :: box
    character(len=32) :: raw
  end type

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

    if (iotype /= "LISTDIRECTED" .and. iotype(1:2) /= "DT") error stop 10
    if (size(v_list) /= 0) error stop 11
    write(unit, '(a)', iostat=iostat, iomsg=iomsg) trim(value%raw)
  end subroutine

  subroutine read_box_formatted(value, unit, iotype, v_list, iostat, iomsg)
    class(box), intent(inout) :: value
    integer, intent(in) :: unit
    character(len=*), intent(in) :: iotype
    integer, intent(in) :: v_list(:)
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg
    character(len=32) :: line

    if (iotype /= "LISTDIRECTED") error stop 12
    if (size(v_list) /= 0) error stop 13
    read(unit, '(a)', iostat=iostat, iomsg=iomsg) line
    value%raw = line
  end subroutine

  subroutine write_box_unformatted(value, unit, iostat, iomsg)
    class(box), intent(in) :: value
    integer, intent(in) :: unit
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    write(unit, iostat=iostat, iomsg=iomsg) value%raw
  end subroutine

  subroutine read_box_unformatted(value, unit, iostat, iomsg)
    class(box), intent(inout) :: value
    integer, intent(in) :: unit
    integer, intent(out) :: iostat
    character(len=*), intent(inout) :: iomsg

    read(unit, iostat=iostat, iomsg=iomsg) value%raw
  end subroutine
end module

program defined_io_derived_roundtrip
  use defined_io_derived_roundtrip_mod
  implicit none

  type(box) :: value
  integer :: unit
  integer :: stat

  value%raw = "Important saved value"
  open(newunit=unit, form="formatted", status="scratch")
  write(unit, *) value
  value%raw = ""
  rewind(unit)
  read(unit, *, iostat=stat) value
  close(unit)
  if (stat /= 0) error stop 1
  if (trim(value%raw) /= "Important saved value") error stop 2

  value%raw = "Formatted value"
  open(newunit=unit, form="formatted", status="scratch")
  write(unit, '(dt)') value
  value%raw = ""
  rewind(unit)
  read(unit, *, iostat=stat) value
  close(unit)
  if (stat /= 0) error stop 3
  if (trim(value%raw) /= "Formatted value") error stop 4

  value%raw = "Unformatted value"
  open(newunit=unit, form="unformatted", status="scratch")
  write(unit) value
  value%raw = ""
  rewind(unit)
  read(unit, iostat=stat) value
  close(unit)
  if (stat /= 0) error stop 5
  if (trim(value%raw) /= "Unformatted value") error stop 6

  print *, "ok"
end program
