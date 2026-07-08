! CHECK: ok
! IR_CHECK: call @afs_list_write_begin
! IR_CHECK: call @afs_list_read_begin
! IR_CHECK: call @afs_write_int
! IR_CHECK: call @afs_write_string
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar10_unformatted_derived
  implicit none

  type :: pair
    integer :: i = 0
    character(len=2) :: tag = '  '
    integer :: j = 0
  end type

  type(pair) :: src(2), got(2)
  integer :: unit_num, ios

  src(1)%i = 3
  src(1)%tag = 'aa'
  src(1)%j = 4
  src(2)%i = 5
  src(2)%tag = 'bb'
  src(2)%j = 6

  got(1)%i = -1
  got(1)%tag = 'xx'
  got(1)%j = -1
  got(2)%i = -1
  got(2)%tag = 'yy'
  got(2)%j = -1

  open(newunit=unit_num, status='scratch', form='unformatted', action='readwrite', iostat=ios)
  if (ios /= 0) error stop 1

  write(unit_num, iostat=ios) src
  if (ios /= 0) error stop 2

  rewind(unit_num, iostat=ios)
  if (ios /= 0) error stop 3

  read(unit_num, iostat=ios) got
  if (ios /= 0) error stop 4

  close(unit_num)

  if (got(1)%i /= 3) error stop 5
  if (got(1)%tag /= 'aa') error stop 6
  if (got(1)%j /= 4) error stop 7
  if (got(2)%i /= 5) error stop 8
  if (got(2)%tag /= 'bb') error stop 9
  if (got(2)%j /= 6) error stop 10

  print '(a)', 'ok'
end program ar10_unformatted_derived
