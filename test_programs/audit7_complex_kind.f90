! Complex kind from iso_c_binding: complex(c_double_complex).
! Verifies c_double_complex resolves as a valid kind selector.
! CHECK: z=
! CHECK: 3.0000000000000
! CHECK: 4.0000000000000
! CHECK: abs(z)=
! CHECK: 5.0000000000000
program test_complex_kind
  use iso_c_binding, only: c_double_complex
  implicit none

  complex(c_double_complex) :: z

  z = (3.0d0, 4.0d0)
  print *, 'z=', z
  print *, 'abs(z)=', abs(z)
end program
