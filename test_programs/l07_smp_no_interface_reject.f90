! l07: a `module procedure` body whose name has no matching interface in
! the ancestor module is rejected (F2008 C1414) instead of silently
! compiling to a dangling procedure.
! FLAGS: --std=f2023
! ERROR_EXPECTED: has no matching MODULE FUNCTION/SUBROUTINE interface
module l07nm
  implicit none
  interface
    module subroutine known()
    end subroutine
  end interface
end module
submodule (l07nm) impl
contains
  module procedure nonexistent
  end procedure
end submodule
