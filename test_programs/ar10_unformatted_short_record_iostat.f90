! CHECK: ok
! IR_CHECK: call @afs_list_read_begin
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_unformatted_short_record_iostat
  implicit none

  integer :: unit_num, ios
  integer :: one, a, b

  one = 5
  a = -1
  b = -1

  open(newunit=unit_num, status='scratch', form='unformatted', action='readwrite', iostat=ios)
  if (ios /= 0) error stop 1

  write(unit_num, iostat=ios) one
  if (ios /= 0) error stop 2

  rewind(unit_num, iostat=ios)
  if (ios /= 0) error stop 3

  read(unit_num, iostat=ios) a, b
  if (ios <= 0) error stop 4

  close(unit_num)
  print '(a)', 'ok'
end program ar10_unformatted_short_record_iostat
