! Audit #4 MINOR-5 — inconsistent const-folding edge cases for
! float division and power.
!
! eval_const_scalar's float arithmetic:
!   * Div returns None on r == 0.0 (avoids fold-to-inf, defers
!     to runtime)
!   * Pow goes through f64::powf unconditionally — `(-2.0)**0.5`
!     folds to NaN with no guard, baked into .data
!
! Inconsistent: one IEEE edge case is deferred, another is
! folded. Pick one. Folding all of them is the simpler rule and
! matches gfortran's behavior on `parameter :: x = 1.0/0.0`.
!
! This test pins the *current* inconsistency: the runtime divide
! produces inf (via libm), but the compile-time pow folds to a
! NaN that's baked in. Either both should fold or neither
! should. Today they don't agree.
!
! XFAIL: audit MINOR-5 (pow folds NaN, div defers — inconsistent)
! CHECK: Infinity
! CHECK: NaN
program audit4_min5_pow_const_fold
  real :: a, b
  a = 1.0 / 0.0    ! runtime → inf
  b = (-1.0) ** 0.5 ! today: const-folds to NaN at compile time
  print *, a
  print *, b
end program
