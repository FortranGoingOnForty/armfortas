! Imported from gcc testsuite gfortran.dg/conditional_4.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2023" } + 7x dg-error (type/kind/rank resolution)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Wanted diagnostic: resolution errors for the condition / arms (the
! condition must be scalar logical; arms need the same type/kind/rank).
! Today the lexer rejects '?' outright, which does not match, so the
! XFAIL fires. The expected substring is deliberately NOT a verbatim
! quote of the gfortran texts kept in the trailing comments below —
! armfortas echoes the offending source line in its diagnostics, and a
! substring present in a trailing comment would self-match. l02 should
! re-word the expectation to its actual diagnostic.
! FLAGS: --std=f2023
! ERROR_EXPECTED: must be scalar
! XFAIL: XFAIL-004 armfortas diagnostic wording differs from gfortran's dg-error substrings (deliberate; revisit if dg wording alignment becomes a goal)
program conditional_resolve
  implicit none
  integer :: i = 42
  integer, parameter :: ucs4 = selected_char_kind('ISO_10646')
  character(kind=1) :: k1 = "k1"
  character(kind=ucs4) :: k4 = "k4"
  integer, dimension(1) :: a_1d
  integer, dimension(1, 1) :: a_2d
  logical :: l1(2)
  integer :: i1(2)
  type :: Point
    real :: x = 0.0
  end type Point
  type(Point) :: p1, p2

  i = (l1 ? 1 : -1) ! original dg-error: "Condition in conditional expression must be a scalar logical"
  i = (i ? 1 : -1) ! original dg-error: "Condition in conditional expression must be a scalar logical"
  i = (i /= 0 ? 1 : "oh no") ! original dg-error: "must have the same declared type"
  i = (i /= 0 ? k1 : k4) ! original dg-error: "must have the same kind parameter"
  i = (i /= 0 ? a_1d : a_2d) ! original dg-error: "must have the same rank"
  p1 = (i /= 0 ? p1 : p2) ! original dg-error: "Sorry, only integer, logical, real, complex and character types are currently supported for conditional expressions"
  i1 = (i /= 0 ? i1 : i1 + 1) ! original dg-error: "Sorry, array is currently unsupported for conditional expressions"
end program conditional_resolve
