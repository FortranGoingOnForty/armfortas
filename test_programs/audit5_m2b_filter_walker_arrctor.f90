! Audit #5 MAJOR-2 (b) — filtered USE ONLY references inside an
! array constructor.
!
! Fixed: check_filtered_in_expr now recurses through
! Expr::ArrayConstructor via a new check_filtered_in_acvalue
! helper that walks both AcValue::Expr and AcValue::ImpliedDo
! (including the implied-do bounds and step). A reference to
! a USE-ONLY-filtered name inside `(/ 1, hidden, 3 /)` now
! produces a compile-time diagnostic.
!
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
