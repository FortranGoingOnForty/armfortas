! Complex abs (modulus): abs((3,4)) = sqrt(9+16) = 5.
! CHECK: 5.0
program t
  implicit none
  complex :: z
  z = (3.0, 4.0)
  print *, abs(z)
end program
