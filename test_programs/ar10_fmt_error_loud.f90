! STDERR_CHECK: Fortran runtime error: format error
! EXIT_CODE: 2
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stderr|exit
program ar10_fmt_error_loud
  implicit none

  print '(L1)', 1
  print *, 'unreachable'
end program ar10_fmt_error_loud
