! A separate module procedure body must have a matching interface in its
! ancestor module (F2008 C1414).
! FLAGS: --std=f2023
! ERROR_EXPECTED: no matching interface in ancestor module
module missing_interface_parent
  implicit none
end module missing_interface_parent

submodule (missing_interface_parent) missing_interface_child
contains
  module subroutine stray()
  end subroutine stray
end submodule missing_interface_child
