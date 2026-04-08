! Audit #4 BLOCKING-1 — implied-do array constructor with negative step.
!
! The B1 fix from audit #3 added a real loop for implied-do, but
! hardcoded `cur <= end` as the termination check. For a descending
! iterator like `(i, i=5,1,-1)`, the very first check fails on
! entry, the body never runs, and the destination array is left
! filled with whatever stack bytes happened to be there.
!
! The regular DO loop lowerer at src/ir/lower.rs:3048 already has
! the right pattern (sign-of-step branch + runtime fallback) — the
! implied-do loop should mirror it.
!
! XFAIL: audit BLOCKING-1 (implied-do negative step skips body)
! CHECK: 5 4 3 2 1
program audit4_b1_implied_do_negative_step
  integer :: a(5)
  integer :: i
  a = [(i, i=5,1,-1)]
  print *, a
end program
