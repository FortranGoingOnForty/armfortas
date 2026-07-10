! CHECK:   1.
! CHECK:  -2.
! CHECK: 4160.0
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar11_f_edit_zero_decimal
  implicit none

  print '(f4.0)', 1.0
  print '(f4.0)', -2.0
  print '(f6.1)', 4160.0
end program ar11_f_edit_zero_decimal
