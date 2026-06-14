! l02a item 3 boundary: an array-valued conditional expression is only
! lowered as a direct assignment RHS (per-arm branch). In any other context
! — here, an operand of a larger expression — there is no descriptor merge,
! so sema must reject it loudly rather than mis-compile. This pins that
! boundary through the real driver (the accept path is covered by
! l02a_array_conditional.f90; the rejection is also unit-tested in sema).
! FLAGS: --std=f2023
! ERROR_EXPECTED: array-valued arms are only supported as the right-hand side
program l02a_array_conditional_reject
  implicit none
  integer :: a(3), b(3), x(3)
  logical :: c
  a = [1, 2, 3]
  b = [10, 20, 30]
  c = .true.
  x = (c ? a : b) + a
end program l02a_array_conditional_reject
