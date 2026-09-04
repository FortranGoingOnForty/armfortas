! CHECK: 42 -7
! IR_CHECK: call @afs_fmt_begin_direct_ex
! IR_CHECK: call @afs_direct_read_begin
! IR_CHECK: call @afs_direct_formatted_read_end
! FILE_MISSING: ar10_direct_formatted_access.dat
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_direct_formatted_access
  implicit none

  integer :: a, b, ios

  open(20, file='ar10_direct_formatted_access.dat', access='direct', &
       form='formatted', recl=12, status='replace', iostat=ios)
  if (ios /= 0) error stop 1

  write(20, '(i5,1x,i5)', rec=3, iostat=ios) 42, -7
  if (ios /= 0) error stop 2
  write(20, '(i5,1x,i5)', rec=1, iostat=ios) 11, 22
  if (ios /= 0) error stop 3

  a = 0
  b = 0
  read(20, '(i5,1x,i5)', rec=3, iostat=ios) a, b
  if (ios /= 0) error stop 4
  close(20, status='delete')

  print '(i0,1x,i0)', a, b
end program ar10_direct_formatted_access
