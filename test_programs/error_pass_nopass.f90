! Type-bound procedure cannot have both PASS and NOPASS.
! ERROR_EXPECTED: both PASS and NOPASS
program t
  implicit none
  type :: mytype
  contains
    procedure, pass, nopass :: sub => my_sub
  end type
contains
  subroutine my_sub(self)
    class(mytype), intent(inout) :: self
  end subroutine
end program
