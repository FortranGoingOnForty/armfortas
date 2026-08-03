! ERROR_EXPECTED: INQUIRE(IOLENGTH=) item may not require defined I/O
module error_inquire_iolength_defined_io_support
  implicit none

  type :: box_t
    integer :: value
  end type box_t

  interface write(unformatted)
    module procedure write_box
  end interface

contains

  subroutine write_box(value, unit, iostat, iomsg)
    class(box_t), intent(in) :: value
    integer, intent(in) :: unit
    integer, intent(out) :: iostat
    character(*), intent(inout) :: iomsg

    write(unit, iostat=iostat, iomsg=iomsg) value%value
  end subroutine write_box

end module error_inquire_iolength_defined_io_support

program error_inquire_iolength_defined_io
  use error_inquire_iolength_defined_io_support
  implicit none

  type(box_t) :: value(2)
  integer :: n

  inquire(iolength=n) value
end program error_inquire_iolength_defined_io
