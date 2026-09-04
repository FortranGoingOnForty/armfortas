! STDERR_CHECK: direct-access WRITE requires REC=
! EXIT_CODE: 2
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stderr|exit
program ar10_direct_access_loud
  implicit none

  open(10, file='ar10_direct_access_loud.dat', access='direct', recl=4, &
       form='unformatted', status='replace')
  write(10) 10
end program ar10_direct_access_loud
