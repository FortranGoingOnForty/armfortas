! Imported from gcc testsuite gfortran.dg/conditional_3.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2023" } + 2x dg-error (malformed conditional syntax)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Wanted diagnostic: a syntax error about the missing colon (gfortran:
! "Expected ':' in conditional expression"). Today the lexer rejects '?'
! outright, which does not match, so the XFAIL fires. The expected
! substring is lowercase on purpose: armfortas echoes the offending
! source line in diagnostics, and the capitalized gfortran quote in the
! trailing comment below must not self-match. l02 should re-word the
! expectation to its actual diagnostic.
! FLAGS: --std=f2023
! ERROR_EXPECTED: expected ':'
! XFAIL: armfortas diagnostic wording differs from gfortran's dg-error substrings (deliberate; revisit if dg wording alignment becomes a goal)
program conditional_syntax
  implicit none
  integer :: i = 42

  i = i > 0 ? 1 : -1 ! original dg-error: "Unclassifiable statement at"
  i = (i > 0 ? 1 -1) ! original dg-error: "Expected ':' in conditional expression"
end program conditional_syntax
