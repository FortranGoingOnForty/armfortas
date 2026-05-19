! CHECK: ok
! IR_CHECK: assign_box_char
! IR_CHECK: concat_box_char
! REPRO_CHECK: run
module shadowed_intrinsic_string_result_assignment_mod
  implicit none

  type :: box
    character(len=:), allocatable :: raw
  end type box

  interface assignment(=)
    module procedure assign_box_char
  end interface

  interface trim
    module procedure trim_box
  end interface

  interface operator(//)
    module procedure concat_box_char
  end interface

  interface operator(==)
    module procedure eq_box_char
  end interface

contains
  pure integer function box_len(value) result(n)
    type(box), intent(in) :: value

    if (allocated(value%raw)) then
      n = len(value%raw)
    else
      n = 0
    end if
  end function box_len

  pure function maybe(value) result(text)
    type(box), intent(in) :: value
    character(len=box_len(value)) :: text

    if (allocated(value%raw)) then
      text = value%raw
    else
      text = ""
    end if
  end function maybe

  pure function trim_box(value) result(trimmed)
    type(box), intent(in) :: value
    type(box) :: trimmed

    trimmed = trim(maybe(value))
  end function trim_box

  pure function concat_box_char(lhs, rhs) result(joined)
    type(box), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    type(box) :: joined

    joined = maybe(lhs) // rhs
  end function concat_box_char

  pure function eq_box_char(lhs, rhs) result(equal)
    type(box), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    logical :: equal

    equal = maybe(lhs) == rhs
  end function eq_box_char

  pure subroutine assign_box_char(lhs, rhs)
    type(box), intent(inout) :: lhs
    character(len=*), intent(in) :: rhs

    lhs%raw = rhs
  end subroutine assign_box_char
end module shadowed_intrinsic_string_result_assignment_mod

program shadowed_intrinsic_string_result_assignment
  use shadowed_intrinsic_string_result_assignment_mod
  implicit none

  type(box) :: value
  type(box) :: trimmed

  value = "  hi  "
  trimmed = trim(value)
  if (.not. allocated(trimmed%raw)) error stop 1
  if (len(trimmed%raw) /= 4) error stop 2
  if (trimmed%raw /= "  hi") error stop 3

  if (.not. (trim(value) // "!" == "  hi!")) error stop 4

  print *, "ok"
end program shadowed_intrinsic_string_result_assignment
