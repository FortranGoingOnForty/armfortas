! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module component_get_string_m
  implicit none

  type :: string_type
    character(len=:), allocatable :: raw
  end type

  interface assignment(=)
    module procedure assign_string_char
  end interface

contains

  subroutine assign_string_char(lhs, rhs)
    type(string_type), intent(inout) :: lhs
    character(len=*), intent(in) :: rhs
    lhs%raw = rhs
  end subroutine

end module

module component_get_list_m
  use component_get_string_m, only: string_type
  implicit none

  type :: list_type
    type(string_type), allocatable :: items(:)
  contains
    procedure :: get
  end type

contains

  function get(list, idx) result(value)
    class(list_type), intent(in) :: list
    integer, intent(in) :: idx
    type(string_type) :: value

    value = list%items(idx)
  end function

end module

program defined_assignment_component_array_get_intrinsic_copy
  use component_get_string_m, only: string_type
  use component_get_list_m, only: list_type
  implicit none

  type(list_type) :: list
  type(string_type) :: value

  allocate(list%items(1))
  list%items(1)%raw = "ok"
  value = list%get(1)

  if (.not. allocated(value%raw)) error stop 1
  if (value%raw /= "ok") error stop 2
  print *, "ok"
end program
