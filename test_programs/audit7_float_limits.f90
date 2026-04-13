! Float constant emission at extreme magnitudes.
! Regression test for large float literal codegen.
! CHECK: tiny=
! CHECK: huge=
program test_float_limits
  implicit none
  real :: tiny_val, huge_val

  tiny_val = 1.0e-30
  huge_val = 1.0e30
  print *, 'tiny=', tiny_val
  print *, 'huge=', huge_val
end program
