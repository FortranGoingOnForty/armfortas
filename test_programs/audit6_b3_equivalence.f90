! Audit #6 BLOCKING-3 — EQUIVALENCE doesn't actually alias.
!
! `equivalence (a, b)` requires a and b to occupy the same
! memory cell. Writing to a should be observable through b.
!
! Observed: writing 42 to a, reading b returns garbage
! (e.g. 15948304). Each variable evidently still has its own
! independent alloca. Fortran 77 baseline feature, in
! CLAUDE.md "in scope and required" list.
!
! CHECK: 42
program audit6_b3_equivalence
  integer :: a, b
  equivalence (a, b)
  a = 42
  print *, b
end program
