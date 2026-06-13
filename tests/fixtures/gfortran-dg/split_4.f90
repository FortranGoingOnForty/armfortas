! Imported from gcc testsuite gfortran.dg/split_4.f90
! (gcc commit b700707a77eeaa1d37f733c4b2d2e242063c29d2).
! Original directives: { dg-do run } { dg-shouldfail "Fortran runtime error" } (BACK with POS at string start)
! Conversion notes: tests/fixtures/gfortran-dg/README.md
! Expected runtime failure. EXIT_CODE 1 follows the armfortas
! runtime-error convention (runtime aborts exit(1)); l04 must confirm
! SPLIT's error semantics when it lands.
! FLAGS: --std=f2023
! EXIT_CODE: 1
! XFAIL: SPLIT POS-out-of-range runtime check not implemented (dg-shouldfail case); armfortas intrinsics do not bounds-check POS. See noted_items.md
program b
  character(len=:), allocatable :: input
  character(len=2) :: set = ', '
  integer :: p
  input = " one,last example,"
  p = 0
  call split(input, set, p, .true.)
end program b
