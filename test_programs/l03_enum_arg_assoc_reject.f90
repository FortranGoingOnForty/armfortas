! l03: argument association — an integer actual against an enumeration
! dummy is an error (before this check the dummy read garbage), and an
! enumeration actual cannot associate with an integer dummy.
! FLAGS: --std=f2023
! ERROR_EXPECTED: dummy argument 'x' has enumeration type 'color'
program l03_enum_arg_assoc_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  call takes_color(2)
contains
  subroutine takes_color(x)
    type(color), intent(in) :: x
    integer :: m
    m = int(x)
  end subroutine
end program l03_enum_arg_assoc_reject
