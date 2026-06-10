! Imported from gcc testsuite gfortran.dg/conditional_9.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2023" } + 3x dg-error (index variable in LOCAL locality-spec)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Wanted diagnostic: the DO CONCURRENT index variable must not appear in
! a LOCAL spec (gfortran: "must not appear in LOCAL locality-spec at",
! truncated in the trailing comments below so the expected substring
! cannot self-match against armfortas's source-line echo). Today the
! lexer rejects '?' first, so the XFAIL fires; after l02 the locality
! diagnostic is still required for this to pass.
! FLAGS: --std=f2023
! ERROR_EXPECTED: LOCAL locality
! XFAIL: f2023 conditional expressions not implemented (l02), and the DO CONCURRENT LOCAL index-variable diagnostic is also missing; see .docs/audits/f2023-feature-matrix.md
implicit none
integer :: i, j
do concurrent (i=(j > 1 ? 0 : 1) : 5) local(j) ! original dg-error: "must not appear in LOCAL ..." (truncated)
end do
do concurrent (i=(.true. ? j : 1) : 5) local(j) ! original dg-error: "must not appear in LOCAL ..." (truncated)
end do
do concurrent (i=(.false. ? 1 : j) : 5) local(j) ! original dg-error: "must not appear in LOCAL ..." (truncated)
end do
end
