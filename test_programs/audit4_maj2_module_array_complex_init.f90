! Audit #4 MAJOR-2 — module array initializers from reshape() or
! implied-do silently zero.
!
! eval_const_array_init only matches Expr::ArrayConstructor with
! literal AcValue::Expr children. It does NOT handle:
!   * reshape(constructor, shape) function calls
!   * AcValue::ImpliedDo
!   * Expr::Name references in element positions
!
! In every unsupported case the global is silently initialized to
! GlobalInit::Zero with no diagnostic. The B2 fix passed its test
! programs only because they happened to use literal-element
! constructors.
!
! Three module variables to check:
!   * arr1 via reshape — values 1..4 in column-major order
!   * arr2 via implied-do — 1..5
!   * arr3 via parameter-relative literals (also tests folder)
!
! XFAIL: audit MAJOR-2 (module array reshape/implied-do drops init)
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
