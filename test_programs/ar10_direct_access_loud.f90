! STDERR_CHECK: OPEN: ACCESS='DIRECT' is not implemented
! EXIT_CODE: 1
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stderr|exit
program ar10_direct_access_loud
  implicit none

  open(10, file='ar10_direct_access_loud.dat', access='direct', recl=4, &
       form='unformatted', status='replace')
  print *, 'bad'
end program ar10_direct_access_loud
