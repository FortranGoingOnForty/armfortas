! Imported from gcc testsuite gfortran.dg/do_concurrent_8_f2023.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=gnu" } + 2x dg-error (variable already in a locality-spec)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! dg-options "-std=gnu" has no armfortas equivalent; REDUCE is F2023, so
! the fixture uses --std=f2023 instead. Wanted diagnostic: a variable may
! not appear in both SHARED and REDUCE locality-specs. Today armfortas
! accepts the file with no diagnostic, so the XFAIL fires.
! FLAGS: --std=f2023
! ERROR_EXPECTED: locality-spec
! XFAIL: XFAIL-005 f2023 DO CONCURRENT REDUCE duplicate-locality diagnostic not implemented (l01); see .docs/audits/f2023-feature-matrix.md
program do_concurrent_complex
  implicit none
  integer :: i, j, k, sum, product
  integer, dimension(10,10,10) :: array
  sum = 0
  product = 1
  do concurrent (i = 1:10) local(j) shared(sum) reduce(+:sum)
    ! original dg-error (applies to a nearby line): "Variable .sum. at .1. has already been specified in a locality-spec"
    do concurrent (j = 1:10) local(k) shared(product) reduce(*:product)
      ! original dg-error (applies to a nearby line): "Variable .product. at .1. has already been specified in a locality-spec"
      do concurrent (k = 1:10)
        array(i,j,k) = i * j * k
        sum = sum + array(i,j,k)
        product = product * array(i,j,k)
      end do
    end do
  end do
  print *, sum, product
end program do_concurrent_complex
