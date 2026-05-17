! CHECK: ok
! IR_CHECK: defined_assign_broadcast_check
! REPRO_CHECK: run
module defined_assignment_array_broadcast_mod
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
end module defined_assignment_array_broadcast_mod

program defined_assignment_array_broadcast
  use defined_assignment_array_broadcast_mod, only : assignment(=), box
  implicit none

  type(box) :: values(2)

  values = "Move This String"

  if (.not. allocated(values(1)%raw)) error stop 1
  if (.not. allocated(values(2)%raw)) error stop 2
  if (values(1)%raw /= "Move This String") error stop 3
  if (values(2)%raw /= "Move This String") error stop 4

  print *, "ok"
end program defined_assignment_array_broadcast
