! A submodule's IMPLICIT NONE statement applies to its contained separate
! module procedure bodies.
! FLAGS: --std=f2023
! ERROR_EXPECTED: variable 'typo' used but not declared (IMPLICIT NONE is active)
module implicit_parent
  interface
    module subroutine run()
    end subroutine run
  end interface
end module implicit_parent

submodule (implicit_parent) implicit_child
  implicit none
contains
  module subroutine run()
    typo = 1
  end subroutine run
end submodule implicit_child
