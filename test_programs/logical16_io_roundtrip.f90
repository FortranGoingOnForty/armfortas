! CHECK: ok
! IR_CHECK: call @afs_write_internal_logical(
! IR_CHECK: call @afs_lst_ia_logical(
! IR_CHECK: call @afs_fmt_push_logical(
! IR_CHECK: call @afs_fmt_read_logical_internal(
! IR_CHECK: call @afs_write_logical_kind(
! IR_CHECK: call @afs_fmt_push_logical(
! IR_CHECK: call @afs_read_logical_kind(
! IR_CHECK: call @afs_fmt_read_logical(
program logical16_io_roundtrip
  implicit none

  type :: guarded_flag
    integer(16) :: before
    logical(16) :: value
    integer(16) :: after
  end type guarded_flag

  character(len=*), parameter :: path = 'logical16_io_roundtrip.tmp'
  character(len=64) :: record
  character(len=:), allocatable :: deferred_record
  integer :: unit, ios
  logical(16) :: source(2), internal_list(2), internal_fmt(2)
  logical(16) :: external_list(2), external_fmt(2), external_unformatted(2), sectioned(4)
  logical(16) :: matrix(3, 3), rank2_values(4)
  type(guarded_flag) :: guarded

  source = [.true., .false.]

  record = ''
  ios = 77
  write(record, *, iostat=ios) source
  if (ios /= 0) error stop 1
  internal_list = [.false., .true.]
  ios = 77
  read(record, *, iostat=ios) internal_list
  if (ios /= 0 .or. .not. internal_list(1) .or. internal_list(2)) error stop 2

  ios = 77
  write(deferred_record, *, iostat=ios) source
  if (ios /= 0) error stop 3
  internal_list = [.false., .true.]
  ios = 77
  read(deferred_record, *, iostat=ios) internal_list
  if (ios /= 0 .or. .not. internal_list(1) .or. internal_list(2)) error stop 4

  record = ''
  ios = 77
  write(record, '(L1,1X,L1)', iostat=ios) source
  if (ios /= 0) error stop 5
  internal_fmt = [.false., .true.]
  ios = 77
  read(record, '(L1,1X,L1)', iostat=ios) internal_fmt
  if (ios /= 0 .or. .not. internal_fmt(1) .or. internal_fmt(2)) error stop 6

  open(newunit=unit, file=path, status='replace', action='readwrite')
  ios = 77
  write(unit, *, iostat=ios) source
  if (ios /= 0) error stop 7
  ios = 77
  write(unit, '(L1,1X,L1)', iostat=ios) source
  if (ios /= 0) error stop 8
  rewind(unit)
  external_list = [.false., .true.]
  ios = 77
  read(unit, *, iostat=ios) external_list
  if (ios /= 0 .or. .not. external_list(1) .or. external_list(2)) error stop 9
  external_fmt = [.false., .true.]
  ios = 77
  read(unit, '(L1,1X,L1)', iostat=ios) external_fmt
  if (ios /= 0 .or. .not. external_fmt(1) .or. external_fmt(2)) error stop 10
  close(unit, status='delete')

  sectioned = [.false., .true., .true., .false.]
  record = 'T F'
  ios = 77
  read(record, *, iostat=ios) sectioned(1:4:2)
  if (ios /= 0) error stop 11
  if (.not. sectioned(1) .or. .not. sectioned(2)) error stop 12
  if (sectioned(3) .or. sectioned(4)) error stop 13

  guarded%before = 111_16
  guarded%value = .false.
  guarded%after = 222_16
  record = 'T'
  ios = 77
  read(record, '(L1)', iostat=ios) guarded%value
  if (ios /= 0 .or. .not. guarded%value) error stop 14
  if (guarded%before /= 111_16 .or. guarded%after /= 222_16) error stop 15

  record = ''
  ios = 77
  write(record, *, iostat=ios) sectioned(1:4:2)
  if (ios /= 0) error stop 16
  internal_list = [.true., .false.]
  ios = 77
  read(record, *, iostat=ios) internal_list
  if (ios /= 0 .or. .not. internal_list(1) .or. internal_list(2)) error stop 17

  ios = 77
  write(deferred_record, *, iostat=ios) sectioned(1:4:2)
  if (ios /= 0) error stop 18
  internal_list = [.false., .true.]
  ios = 77
  read(deferred_record, *, iostat=ios) internal_list
  if (ios /= 0 .or. .not. internal_list(1) .or. internal_list(2)) error stop 19

  matrix = .false.
  matrix(1, 1) = .true.
  matrix(3, 3) = .true.
  record = ''
  ios = 77
  write(record, *, iostat=ios) matrix(1:3:2, 1:3:2)
  if (ios /= 0) error stop 20
  rank2_values = .true.
  ios = 77
  read(record, *, iostat=ios) rank2_values
  if (ios /= 0) error stop 21
  if (.not. rank2_values(1) .or. rank2_values(2) .or. rank2_values(3) .or. &
      .not. rank2_values(4)) error stop 22

  ios = 77
  write(deferred_record, *, iostat=ios) matrix(1:3:2, 1:3:2)
  if (ios /= 0) error stop 23
  rank2_values = .true.
  ios = 77
  read(deferred_record, *, iostat=ios) rank2_values
  if (ios /= 0) error stop 24
  if (.not. rank2_values(1) .or. rank2_values(2) .or. rank2_values(3) .or. &
      .not. rank2_values(4)) error stop 25

  open(newunit=unit, file=path, status='replace', action='readwrite', form='unformatted')
  ios = 77
  write(unit, iostat=ios) source
  if (ios /= 0) error stop 26
  guarded%before = 333_16
  guarded%value = .true.
  guarded%after = 444_16
  ios = 77
  write(unit, iostat=ios) guarded%value
  if (ios /= 0) error stop 27
  rewind(unit)

  external_unformatted = [.false., .true.]
  ios = 77
  read(unit, iostat=ios) external_unformatted
  if (ios /= 0 .or. .not. external_unformatted(1) .or. external_unformatted(2)) &
    error stop 28

  guarded%before = 333_16
  guarded%value = .false.
  guarded%after = 444_16
  ios = 77
  read(unit, iostat=ios) guarded%value
  if (ios /= 0 .or. .not. guarded%value) error stop 29
  if (guarded%before /= 333_16 .or. guarded%after /= 444_16) error stop 30
  close(unit, status='delete')

  print *, 'ok'
end program logical16_io_roundtrip
