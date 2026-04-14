! IMPLICIT NONE inside a MODULE applies to module procedures and
! their bodies. Audit-level: a module procedure that references an
! undeclared name must error at compile time, not silently produce
! a zero-init local that the optimiser will fold to 0.
! ERROR_EXPECTED: not declared
module mod_strict
  implicit none
contains
  subroutine bad()
    j = 99
  end subroutine
end module
program t
  use mod_strict
  call bad()
end program
