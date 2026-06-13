! Imported from gcc testsuite gfortran.dg/system_clock_4.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do compile } { dg-options "-std=f2023" } + 10x dg-error (kind smaller than default integer / different kind)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Wanted diagnostic: a SYSTEM_CLOCK argument-restriction error (kind
! smaller than default integer / mixed integer kinds). Today armfortas
! accepts every call here with no diagnostic, so the XFAIL fires.
! FLAGS: --std=f2023
! ERROR_EXPECTED: SYSTEM_CLOCK
! PR fortran/112609 - F2023 restrictions on integer arguments to SYSTEM_CLOCK

program p
  implicit none
  integer    :: i,  j,  k
  integer(2) :: i2, j2, k2
  integer(8) :: i8, j8, k8
  real       :: x

  call system_clock(count=i2)      ! original dg-error: "kind smaller than default integer"
  call system_clock(count_rate=j2) ! original dg-error: "kind smaller than default integer"
  call system_clock(count_max=k2)  ! original dg-error: "kind smaller than default integer"

  call system_clock(count=i8,count_rate=x,count_max=k8)
  call system_clock(count=i, count_rate=j8)     ! original dg-error: "different kind"
  call system_clock(count=i8,count_rate=j)      ! original dg-error: "different kind"
  call system_clock(count=i, count_max=k8)      ! original dg-error: "different kind"
  call system_clock(count=i8,count_max=k)       ! original dg-error: "different kind"
  call system_clock(count_rate=j, count_max=k8) ! original dg-error: "different kind"
  call system_clock(count_rate=j8,count_max=k)  ! original dg-error: "different kind"
  call system_clock(i,x,k8)                     ! original dg-error: "different kind"
end
