! Imported from gcc testsuite gfortran.dg/conditional_7.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2023" } (bare module, not runnable)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! FLAGS: --std=f2023
! EXIT_CODE: 0
module m
  contains
    function f(n) result(str)
      integer, value :: n
      character(len=(n > 5 ? n : 5)) :: str
      str = ""
      str(1:5) = "abcde"
    end
end

! Appended for the armfortas import: the original is a bare-module compile
! test; a trivial main program makes it runnable for the EXIT_CODE oracle.
program conditional_7_main
end program conditional_7_main
