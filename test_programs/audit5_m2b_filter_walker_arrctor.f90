! Audit #5 MAJOR-2 (b) — filtered USE ONLY references inside an
! array constructor are silently replaced with zero.
!
! check_filtered_in_expr handles BinaryOp/UnaryOp/Paren/FunctionCall
! /Name but wildcards through Expr::ArrayConstructor and
! AcValue::Expr / AcValue::ImpliedDo. So `(/ 1, hidden, 3 /)`
! produces `1 0 3` at runtime without diagnosing.
!
! Expected: compile-time error mentioning `hidden`.
!
! XFAIL: audit5 MAJOR-2 (filter walker doesn't cover ArrayConstructor)
! ERROR_EXPECTED: hidden
module audit5_m2b_mod
  integer :: visible = 1
  integer :: hidden = 999
end module audit5_m2b_mod

program audit5_m2b_filter_walker_arrctor
  use audit5_m2b_mod, only: visible
  integer :: a(3)
  a = (/ 1, hidden, 3 /)
  print *, a
end program
