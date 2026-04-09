! Audit #6 probe — USE ONLY filter walker covers FORALL bodies.
!
! Companion to the audit5 MAJOR-2 fix that made the filter
! walker exhaustive. This program references a USE-ONLY-filtered
! name from inside a FORALL construct body — a previous walker
! version (pre-audit5) silently lowered the reference to
! const_int 0; the exhaustive walker now produces a compile-
! time diagnostic.
!
! ERROR_EXPECTED: hidden
module audit6_filter_forall_mod
  integer :: visible = 1
  integer :: hidden = 999
end module audit6_filter_forall_mod

program audit6_filter_forall
  use audit6_filter_forall_mod, only: visible
  integer :: a(3), i
  a = 0
  forall (i = 1:3) a(i) = hidden
  print *, a
end program
