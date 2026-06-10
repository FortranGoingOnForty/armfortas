! Imported from gcc testsuite gfortran.dg/conditional_8.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do run } { dg-options "-std=f2023" }
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! FLAGS: --std=f2023
! EXIT_CODE: 0
! XFAIL: f2023 conditional expressions not implemented (l02); see .docs/audits/f2023-feature-matrix.md
implicit none
integer :: aa(2)
aa = [1, 2]

print *, (aa(1) > 0 ? aa(2) : g())
contains
integer function g()
  allocatable :: g
  error stop "should not be called"
  g = 3
end
end
