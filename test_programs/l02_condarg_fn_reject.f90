! l02: conditional arguments to USER procedures are CALL-only so far —
! a function-reference conditional would degrade to a value temp and
! silently break INTENT(OUT)/INOUT. Loud rejection until the fn-call
! path selects associations too (matrix-noted).
! FLAGS: --std=f2023
! ERROR_EXPECTED: conditional arguments to user procedures are only supported in CALL
program l02_condarg_fn_reject
  implicit none
  integer :: a, r
  a = 3
  r = twice((a > 0 ? a : 1))
  print *, r
contains
  integer function twice(x)
    integer, intent(in) :: x
    twice = 2 * x
  end function twice
end program l02_condarg_fn_reject
