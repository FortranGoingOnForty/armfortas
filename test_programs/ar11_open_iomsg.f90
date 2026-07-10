! CHECK: iostat nonzero
! CHECK: iomsg assigned
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar11_open_iomsg
  implicit none

  integer :: ios, unit_num
  character(len=256) :: msg

  msg = 'SENTINEL-UNTOUCHED'
  open(newunit=unit_num, file='/tmp/afs_missing_open_iomsg_parent_963852741/file.txt', &
       status='old', action='read', iostat=ios, iomsg=msg)

  if (ios == 0) error stop 1
  if (trim(msg) == 'SENTINEL-UNTOUCHED') error stop 2
  if (len_trim(msg) == 0) error stop 3

  print *, 'iostat nonzero'
  print *, 'iomsg assigned'
end program ar11_open_iomsg
