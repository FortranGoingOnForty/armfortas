! Malformed dynamic FORMAT values report failure instead of being normalized
! into consuming descriptors. A valid dynamic FORMAT still works after each
! failure, proving that rejected text is not cached as a partial parse.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_fmt_begin_internal_ex(
! IR_CHECK: call @afs_fmt_read_int_internal(
program ar44_malformed_dynamic_format
  implicit none

  character(len=64) :: fmt
  character(len=5) :: input
  character(len=64) :: message
  character(len=64) :: record
  integer :: ios
  integer :: value

  fmt = '(F8)'
  message = 'sentinel'
  record = 'unchanged'
  ios = -99
  write(record, fmt, iostat=ios, iomsg=message) 1.25
  if (ios == 0) error stop 1
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 2

  fmt = '(2(I3)'
  message = 'sentinel'
  ios = -99
  write(record, fmt, iostat=ios, iomsg=message) 7
  if (ios == 0) error stop 3
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 4

  fmt = 'I3'
  message = 'sentinel'
  ios = -99
  write(record, fmt, iostat=ios, iomsg=message) 7
  if (ios == 0) error stop 5
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 6

  fmt = '(Q5)'
  input = '   42'
  message = 'sentinel'
  value = 1234
  ios = -99
  read(input, fmt, iostat=ios, iomsg=message) value
  if (ios == 0) error stop 7
  if (value /= 1234) error stop 8
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 9

  fmt = '((((((((((((((((I3))))))))))))))))'
  message = 'sentinel'
  ios = -99
  write(record, fmt, iostat=ios, iomsg=message) 7
  if (ios /= 0) error stop 10
  if (record(1:3) /= '  7') error stop 11

  fmt = '(I5)'
  message = 'sentinel'
  value = 0
  ios = -99
  read(input, fmt, iostat=ios, iomsg=message) value
  if (ios /= 0) error stop 12
  if (value /= 42) error stop 13

  print '(a)', 'ok'
end program ar44_malformed_dynamic_format
