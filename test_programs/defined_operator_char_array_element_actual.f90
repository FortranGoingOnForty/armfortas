! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module defined_operator_char_array_element_m
  implicit none

  type :: string_type
    character(len=:), allocatable :: raw
  end type string_type

  interface operator(/=)
    module procedure ne_string_char
  end interface

contains

  pure logical function ne_string_char(lhs, rhs)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs

    ne_string_char = lhs%raw /= rhs
  end function ne_string_char

end module defined_operator_char_array_element_m

program defined_operator_char_array_element_actual
  use defined_operator_char_array_element_m, only: string_type, operator(/=)
  implicit none

  type(string_type) :: value
  character(len=4) :: words(2)

  value%raw = '#1  '
  words(1) = '#1'
  words(2) = '#2'

  call check(value, words)
  print *, "ok"

contains

  subroutine check(lhs, rhs)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs(:)

    if (lhs /= rhs(1)) error stop 1
    if (.not. (lhs /= rhs(2))) error stop 2
  end subroutine check

end program defined_operator_char_array_element_actual
