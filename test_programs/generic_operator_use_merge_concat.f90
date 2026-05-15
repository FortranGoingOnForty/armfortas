! CHECK: ok
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|obj|repro
module generic_operator_string_m
  implicit none

  type :: string_type
    integer :: marker = 0
  end type

  interface operator(//)
    module procedure concat_string_char
  end interface

contains

  function concat_string_char(lhs, rhs) result(out)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    type(string_type) :: out

    out%marker = lhs%marker + len(rhs) + 100
  end function

end module

module generic_operator_list_m
  implicit none

  type :: list_type
    integer :: count = 0
  end type

  interface operator(//)
    module procedure append_char
  end interface

contains

  function append_char(lhs, rhs) result(out)
    type(list_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    type(list_type) :: out

    if (len(rhs) <= 0) error stop 1
    out%count = lhs%count + 1
  end function

end module

program generic_operator_use_merge_concat
  use generic_operator_string_m, only: string_type, operator(//)
  use generic_operator_list_m, only: list_type, operator(//)
  implicit none

  type(list_type) :: items
  type(string_type) :: unused

  unused%marker = 7
  items = items // "abc"

  if (items%count /= 1) error stop 2
  print *, "ok"
end program
