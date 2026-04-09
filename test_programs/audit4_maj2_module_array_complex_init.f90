! Audit #4 MAJOR-2 — module array initializers via reshape() or
! implied-do now const-fold correctly.
!
! Fixed: eval_const_array_init now uses two helpers that handle
! the full constructor grammar:
!
!   * collect_const_array_scalars walks the expression
!     recursively, supporting Expr::ArrayConstructor (literal
!     elements), Expr::FunctionCall("reshape", [src, shape])
!     (passes through src elements unchanged — Fortran's
!     reshape preserves linearization order even when permuting
!     dims), and parameter references via the C1 const map.
!   * collect_ac_value handles each AcValue, including
!     implied-do iterators with const bounds. Implied-do binds
!     its variable in a local param_consts overlay so the
!     inner expression can reference it (`(i*i, i=1,5)`).
!
! Three module variables verified:
!   * arr1 via reshape — values 1..4
!   * arr2 via implied-do — 1..5
!   * arr3 via parameter-relative literals (already worked
!     after C1, kept as a regression pin)
!
! CHECK: 1 2 3 4
! CHECK: 1 2 3 4 5
! CHECK: 100 200 300
program audit4_maj2_module_array_complex_init
  use audit4_maj2_mod
  print *, arr1
  print *, arr2
  print *, arr3
end program

module audit4_maj2_mod
  integer :: arr1(2,2) = reshape([1,2,3,4], [2,2])
  integer :: arr2(5) = [(i, i=1,5)]
  integer, parameter :: base = 100
  integer :: arr3(3) = [base, base*2, base*3]
end module audit4_maj2_mod
