! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_rw_rewind
  implicit none

  character(len=*), parameter :: path = 'ar10_rw_rewind.dat'
  integer :: unit_num, ios, got

  open(newunit=unit_num, file=path, status='replace', iostat=ios)
  if (ios /= 0) error stop 1

  write(unit_num, *, iostat=ios) 42
  if (ios /= 0) error stop 2

  rewind(unit_num, iostat=ios)
  if (ios /= 0) error stop 3

  got = -1
  read(unit_num, *, iostat=ios) got
  if (ios /= 0) error stop 4
  if (got /= 42) error stop 5

  close(unit_num, status='delete')
  print '(a)', 'ok'
end program ar10_rw_rewind
