! Test ComplexLiteral expression lowering (BLOCKING fix)
! Previously (re,im) literals silently became 0. Now they store
! real and imaginary parts correctly, and arithmetic works.
program complex_literal
  complex :: z1, z2, z3

  ! Basic literal
  z1 = (3.0, 4.0)
  print *, z1

  ! Negative imaginary
  z2 = (1.0, -2.0)
  print *, z2

  ! Zero
  z1 = (0.0, 0.0)
  print *, z1

  ! Complex addition: (1+3, 2+4) = (4, 6)
  z3 = (1.0, 2.0) + (3.0, 4.0)
  print *, z3

  ! Complex subtraction: (5-3, 7-2) = (2, 5)
  z3 = (5.0, 7.0) - (3.0, 2.0)
  print *, z3

  ! Complex multiplication: (1*3 - 2*4, 1*4 + 2*3) = (-5, 10)
  z3 = (1.0, 2.0) * (3.0, 4.0)
  print *, z3
end program complex_literal
! CHECK: (   3.0000000E0,   4.0000000E0)
! CHECK: (   1.0000000E0,  -2.0000000E0)
! CHECK: (   0.0000000E0,   0.0000000E0)
! CHECK: (   4.0000000E0,   6.0000000E0)
! CHECK: (   2.0000000E0,   5.0000000E0)
! CHECK: (  -5.0000000E0,   1.0000000E1)
