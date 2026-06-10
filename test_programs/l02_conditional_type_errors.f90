! l02: conditional constraint errors — non-logical condition and
! mismatched arm types (gfortran conditional_4.f90 class).
! FLAGS: --std=f2023
! ERROR_EXPECTED: must be a scalar LOGICAL
program l02_conditional_type_errors
  implicit none
  integer :: x
  x = (1 ? 2 : 3)
  print *, x
end program l02_conditional_type_errors
