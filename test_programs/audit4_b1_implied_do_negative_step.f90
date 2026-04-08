! Audit #4 BLOCKING-1 — implied-do array constructor with negative step.
!
! Fixed: store_ac_implied_do now mirrors the regular DO lowerer's
! sign-of-step pattern (compile-time const → pick Le/Ge directly,
! runtime → branch on sign at the check site). Previously the
! check was hardcoded `Le`, so `(i, i=5,1,-1)` exited on the
! first iteration without writing anything to the destination.
!
! CHECK: 5 4 3 2 1
program audit4_b1_implied_do_negative_step
  integer :: a(5)
  integer :: i
  a = [(i, i=5,1,-1)]
  print *, a
end program
