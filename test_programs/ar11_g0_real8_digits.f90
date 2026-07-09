! CHECK: 0.333333343
! CHECK: 0.33333333333333331
! CHECK: 100.00000000000000
! CHECK: 3.1415926535897931
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar11_g0_real8_digits
  implicit none

  real(4) :: single
  real(8) :: wide

  single = 1.0 / 3.0
  wide = 1.0d0 / 3.0d0

  print '(G0)', single
  print '(G0)', wide
  print '(G0)', 100.0d0
  print '(G0)', 4.0d0 * atan(1.0d0)
end program ar11_g0_real8_digits
