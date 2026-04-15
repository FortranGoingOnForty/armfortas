! audit31 Finding 17: a submodule providing the implementation of a
! parent module's separate-module function parsed cleanly but emitted
! no IR, so the linker reported `Undefined symbols: _compute`.  The
! lowerer's lower_unit only matched ProgramUnit::Module — Submodule
! fell through the catch-all `_ => {}` arm and produced no body.
! Fix: extend lower_unit and the supporting collect_* / host-ref
! walkers to descend into Submodule contains, and register the
! submodule's own decls as globals under the parent module's name so
! its contained procedures can resolve them via the same global
! lookup. Task #498.
! CHECK: 50
module base
  implicit none
  interface
    module function compute(x) result(r)
      integer, intent(in) :: x
      integer :: r
    end function
  end interface
end module

submodule (base) impl
contains
  module function compute(x) result(r)
    integer, intent(in) :: x
    integer :: r
    r = x * 10
  end function
end submodule

program audit31_submodule
  use base
  implicit none
  print *, compute(5)
end program
