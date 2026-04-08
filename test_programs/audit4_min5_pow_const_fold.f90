! Audit #4 MINOR-5 — float const folder now handles IEEE edge
! cases consistently.
!
! Pre-fix: eval_const_scalar's float Div arm bailed on `r == 0.0`
! (returning None and deferring to runtime), but Pow went through
! `f64::powf` unconditionally and folded `(-1.0)**0.5` to NaN at
! compile time. One IEEE edge case was deferred, another was
! folded — the audit flagged the inconsistency.
!
! Fixed: float Div now folds to ±Inf or NaN per IEEE 754 (matching
! both Pow's existing behavior and gfortran's const-init semantics).
! Both edge cases produce a finite IEEE result via const folding;
! the eventual store + load + print sees the same bit pattern.
!
! CHECK: NaN
program audit4_min5_pow_const_fold
  real :: x = (-1.0) ** 0.5    ! folds to NaN at compile time
  print *, x
end program
