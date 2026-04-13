! Parameter used as kind selector in type declaration.
! Regression test for named-constant kind resolution.
! CHECK: real(k8) x=     2.718281828459045E0
program test_param_kind
  implicit none
  integer, parameter :: k8 = 8
  real(k8) :: x

  x = 2.718281828459045d0
  print *, 'real(k8) x=', x
end program
