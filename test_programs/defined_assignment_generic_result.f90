! CHECK: ok
! IR_CHECK: __prog_defined_assignment_generic_result
! REPRO_CHECK: run
module defined_assignment_generic_result_core
  implicit none

  type :: box
    character(len=:), allocatable :: raw
  end type box

  interface assignment(=)
    module procedure assign_box_char
  end interface

contains
  elemental subroutine assign_box_char(lhs, rhs)
    type(box), intent(inout) :: lhs
    character(len=*), intent(in) :: rhs

    lhs%raw = rhs
  end subroutine assign_box_char

  function char_box(value) result(text)
    type(box), intent(in) :: value
    character(len=len(value%raw)) :: text

    text = value%raw
  end function char_box
end module defined_assignment_generic_result_core

module defined_assignment_generic_result_ops
  use defined_assignment_generic_result_core, only : box, assignment(=), char_box
  implicit none

  interface strip
    module procedure strip_box
    module procedure strip_text
  end interface

contains
  function strip_box(value) result(out)
    type(box), intent(in) :: value
    type(box) :: out

    out = strip_text(char_box(value))
  end function strip_box

  function strip_text(text) result(out)
    character(len=*), intent(in) :: text
    character(len=:), allocatable :: out

    out = trim(adjustl(text))
  end function strip_text
end module defined_assignment_generic_result_ops

program defined_assignment_generic_result
  use defined_assignment_generic_result_core, only : box, assignment(=)
  use defined_assignment_generic_result_ops, only : strip
  implicit none

  type(box) :: value
  type(box) :: stripped

  value = "   hello   "
  stripped = strip(value)

  if (.not. allocated(stripped%raw)) error stop 1
  if (len(stripped%raw) /= 5) error stop 2
  if (stripped%raw /= "hello") error stop 3

  print *, "ok"
end program defined_assignment_generic_result
