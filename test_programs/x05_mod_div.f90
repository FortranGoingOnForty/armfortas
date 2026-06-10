! Sprint x05 curated program: MOD and division, including negative
! operands — the idiv remainder takes the dividend's sign, which is
! exactly Fortran MOD semantics.
! CHECK: 2
! CHECK: -2
! CHECK: 14
! CHECK: -14
program x05_mod_div
  implicit none
  integer :: a, b
  a = 44
  b = 3
  print *, mod(a, b)
  print *, mod(-a, b)
  print *, a / b
  print *, (-a) / b
end program x05_mod_div
