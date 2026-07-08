! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_fmt_error_iostat
  implicit none

  integer :: ios
  character(len=40) :: msg

  ios = 0
  msg = ''
  write (*, '(L1)', iostat=ios, iomsg=msg) 1
  if (ios == 0) error stop 1
  if (index(msg, 'format error') == 0) error stop 2
  print *, 'ok'
end program ar10_fmt_error_iostat
