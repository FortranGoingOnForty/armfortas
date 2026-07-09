! CHECK: rejected
! IR_CHECK: call @afs_open
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_direct_access
  implicit none

  character(len=*), parameter :: path = 'ar10_direct_access_reject.dat'
  integer :: unit_num, ios
  logical :: exists

  unit_num = -777
  ios = -1
  open(newunit=unit_num, file=path, access='direct', recl=4, &
       form='unformatted', status='replace', iostat=ios)

  if (ios == 0) then
    write(unit_num, rec=1, iostat=ios) 10
    error stop 1
  end if

  inquire(file=path, exist=exists)
  if (exists) error stop 2

  print '(a)', 'rejected'
end program ar10_direct_access
