! CHECK: ok
! IR_CHECK: __prog_associated_byref_target_self
! REPRO_CHECK: run
module associated_byref_target_self_mod
  implicit none

  type :: box
    integer :: value = 0
  end type box

contains
  subroutine keep_if_self(from, to)
    type(box), intent(inout), target :: from
    type(box), intent(inout), target :: to
    type(box), pointer :: fromp

    fromp => from
    if (.not. associated(fromp, to)) then
      to%value = -1
    end if
  end subroutine keep_if_self
end module associated_byref_target_self_mod

program associated_byref_target_self
  use associated_byref_target_self_mod, only : box, keep_if_self
  implicit none

  type(box) :: value

  value%value = 42
  call keep_if_self(value, value)

  if (value%value /= 42) error stop 1
  print *, "ok"
end program associated_byref_target_self
