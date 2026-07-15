! STDERR_CHECK: Fortran runtime error: WRITE failed with IOSTAT=91
! EXIT_CODE: 2
module write_defined_unhandled_error_support
  implicit none

  type :: box
    integer :: value
  end type box

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

    iostat = 91
    iomsg = 'defined write failed'
  end subroutine write_box

end module write_defined_unhandled_error_support

program write_defined_unhandled_error
  use write_defined_unhandled_error_support
  implicit none

  type(box) :: object

  object%value = 7
  write(*, *) object
  error stop 1
end program write_defined_unhandled_error
