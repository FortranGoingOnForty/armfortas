program ar12_sign_negzero
  implicit none
  integer :: n
  real :: z
  real(8) :: zd

  n = command_argument_count()
  z = -real(n)
  zd = -real(n, 8)
  print '(a,z8.8)', 'bits=', transfer(z, 1)
! CHECK: bits=80000000
  print '(a,f6.2)', 's_var=', sign(1.0, z)
! CHECK: s_var= -1.00
  print '(a,f6.2)', 's_lit=', sign(1.0, -0.0)
! CHECK: s_lit= -1.00
  print '(a,f6.2)', 's_d=', real(sign(1.0_8, zd))
! CHECK: s_d= -1.00
end program ar12_sign_negzero
