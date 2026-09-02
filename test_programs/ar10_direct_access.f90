! CHECK: 30 10 20 DIRECT UNFORMATTED 4
! IR_CHECK: call @afs_open
! IR_CHECK: call @afs_direct_write_begin
! IR_CHECK: call @afs_direct_read_begin
! FILE_MISSING: ar10_direct_access.dat
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_direct_access
  implicit none

  character(len=*), parameter :: path = 'ar10_direct_access.dat'
  character(len=16) :: access_mode, form_mode
  integer :: unit_num, ios, a, b, c, record_len

  unit_num = -777
  ios = -1
  open(newunit=unit_num, file=path, access='direct', recl=4, &
       form='unformatted', status='replace', iostat=ios)
  if (ios /= 0) error stop 1

  write(unit_num, rec=3, iostat=ios) 30
  if (ios /= 0) error stop 2
  write(unit_num, rec=1, iostat=ios) 10
  if (ios /= 0) error stop 3
  write(unit_num, rec=2, iostat=ios) 20
  if (ios /= 0) error stop 4

  a = 0
  b = 0
  c = 0
  read(unit_num, rec=3, iostat=ios) a
  if (ios /= 0) error stop 5
  read(unit_num, rec=1, iostat=ios) b
  if (ios /= 0) error stop 6
  read(unit_num, rec=2, iostat=ios) c
  if (ios /= 0) error stop 7

  access_mode = ''
  form_mode = ''
  record_len = -1
  inquire(unit=unit_num, access=access_mode, form=form_mode, recl=record_len, iostat=ios)
  if (ios /= 0) error stop 8
  close(unit_num, status='delete')

  print '(3(i0,1x),a,1x,a,1x,i0)', a, b, c, trim(access_mode), &
        trim(form_mode), record_len
end program ar10_direct_access
