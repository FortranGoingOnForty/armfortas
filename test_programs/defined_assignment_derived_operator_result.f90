! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module defined_assignment_derived_operator_result_m
  implicit none

  type :: string_type
    character(len=:), allocatable :: raw
  end type string_type

  interface assignment(=)
    module procedure assign_string_char
  end interface

  interface operator(//)
    module procedure concat_string_char
  end interface

contains

  subroutine assign_string_char(lhs, rhs)
    type(string_type), intent(inout) :: lhs
    character(len=*), intent(in) :: rhs

    lhs%raw = rhs
  end subroutine assign_string_char

  function concat_string_char(lhs, rhs) result(out)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    type(string_type) :: out

    out%raw = lhs%raw // rhs
  end function concat_string_char

end module defined_assignment_derived_operator_result_m

program defined_assignment_derived_operator_result
  use defined_assignment_derived_operator_result_m, only: string_type, assignment(=), operator(//)
  implicit none

  type(string_type) :: value

  value = "Hello, "
  value = value // "World!"

  if (.not. allocated(value%raw)) error stop 1
  if (len(value%raw) /= 13) error stop 2
  if (value%raw /= "Hello, World!") error stop 3

  print *, "ok"
end program defined_assignment_derived_operator_result
