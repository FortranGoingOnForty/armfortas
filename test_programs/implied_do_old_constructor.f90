! Implied-do inside the old-style `(/ ... /)` array constructor.
! The slash-form parser used parse_expr_bp which can't see the
! `var = start, end` inside a parenthesised AcValue — we now route
! each value through parse_ac_value_bracketed so both this and
! `[ ... ]` accept the same AcValue grammar.
! CHECK: 1 2 3 4 5
program t
  implicit none
  integer :: a(5)
  integer :: i
  a = (/ (i, i=1,5) /)
  print *, a
end program
