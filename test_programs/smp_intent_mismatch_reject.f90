! A separate module procedure body must preserve each dummy argument's
! INTENT characteristic from the ancestor interface (F2008 C1418).
! FLAGS: --std=f2023
! ERROR_EXPECTED: INTENT(OUT), which does not match INTENT(IN)
module intent_parent
  implicit none
  interface
    module subroutine update(value)
      integer, intent(in) :: value
    end subroutine update
  end interface
end module intent_parent

submodule (intent_parent) intent_child
contains
  module subroutine update(value)
    integer, intent(out) :: value
  end subroutine update
end submodule intent_child
