! Imported from gcc testsuite gfortran.dg/conditional_5.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2018" } + dg-error "Fortran 2023: Conditional expression"
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Wanted diagnostic: the require_std gate shape, e.g.
! "... requires --std=F2023 or later". Today the lexer rejects '?'
! under every std, which does not match, so the XFAIL fires.
! FLAGS: --std=f2018
! ERROR_EXPECTED: requires --std=F2023
program conditional_std
  implicit none
  integer :: i = 42
  i = (i > 0 ? 1 : -1) ! original dg-error: "Fortran 2023: Conditional expression at"
end program conditional_std
