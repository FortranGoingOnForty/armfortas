! l02: conditional expressions are F2023; older std levels reject
! with a conformance diagnostic (gfortran conditional_5.f90).
! FLAGS: --std=f2018
! ERROR_EXPECTED: conditional expression requires --std=F2023
! ERROR_SPAN: 9:7
program l02_conditional_std_reject
  implicit none
  integer :: x
  x = (1 > 0 ? 1 : 2)
  print *, x
end program l02_conditional_std_reject
