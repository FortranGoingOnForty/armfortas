! CHECK:   12.346E+03
! CHECK:   00.000E+00
! CHECK:   10.000E+04
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar11_kp_scale_e_digits
  implicit none

  print '(2pe12.4)', 12345.678
  print '(2pe12.4)', 0.0
  print '(2pe12.4)', 99999.99
end program ar11_kp_scale_e_digits
