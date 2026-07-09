! STDERR_CHECK: STOP char stop code message
! EXIT_CODE: 0
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar11_stop_char_code
  implicit none

  stop 'char stop code message'
end program ar11_stop_char_code
