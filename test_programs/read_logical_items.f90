! CHECK: ok
! IR_CHECK: call @afs_write_logical_kind(
! IR_CHECK: call @afs_read_logical_kind(
program read_logical_items
  implicit none

  type :: flag_record
    logical(1) :: narrow
    integer(1) :: guard
    logical :: ordinary
  end type flag_record

  character(len=32) :: record
  integer :: unit, ios
  logical :: internal_list(2), internal_fmt(2)
  logical :: external_list(2), external_fmt(2)
  logical :: sectioned(3)
  logical(1) :: narrow1
  integer(1) :: guard1
  logical(2) :: narrow2
  integer(2) :: guard2
  logical(8) :: wide
  integer(8) :: guard8
  logical(1) :: raw1
  logical(2) :: raw2
  logical(4) :: raw4
  logical(8) :: raw8
  integer(1) :: raw_guard1
  integer(2) :: raw_guard2
  integer(4) :: raw_guard4
  integer(8) :: raw_guard8
  type(flag_record) :: value

  internal_list = .false.
  record = 'T, .FALSE.'
  ios = 77
  read(record, *, iostat=ios) internal_list
  if (ios /= 0 .or. .not. internal_list(1) .or. internal_list(2)) error stop 1

  internal_fmt = .true.
  record = '.TRUE. .FALSE.'
  ios = 77
  read(record, '(L6,1X,L7)', iostat=ios) internal_fmt
  if (ios /= 0 .or. .not. internal_fmt(1) .or. internal_fmt(2)) error stop 2

  sectioned = .false.
  record = 'T T'
  ios = 77
  read(record, *, iostat=ios) sectioned(1:3:2)
  if (ios /= 0 .or. .not. sectioned(1) .or. sectioned(2) .or. &
      .not. sectioned(3)) error stop 3

  narrow1 = .false.
  guard1 = 11
  narrow2 = .true.
  guard2 = 22
  wide = .false.
  guard8 = 88
  record = 'T F .TRUE.'
  ios = 77
  read(record, *, iostat=ios) narrow1, narrow2, wide
  if (ios /= 0 .or. .not. narrow1 .or. narrow2 .or. .not. wide) error stop 4
  if (guard1 /= 11 .or. guard2 /= 22 .or. guard8 /= 88) error stop 5

  value%narrow = .false.
  value%guard = 33
  value%ordinary = .true.
  record = 'T F'
  ios = 77
  read(record, *, iostat=ios) value%narrow, value%ordinary
  if (ios /= 0 .or. .not. value%narrow .or. value%ordinary) error stop 6
  if (value%guard /= 33) error stop 7

  external_list = .false.
  external_fmt = .true.
  value%narrow = .true.
  value%guard = 0
  value%ordinary = .false.
  open(newunit=unit, status='scratch', action='readwrite')
  write(unit, '(A)') 'T, .FALSE.'
  write(unit, '(A)') '.TRUE. .FALSE.'
  write(unit, '(A)') 'F 44 T'
  rewind(unit)
  ios = 77
  read(unit, *, iostat=ios) external_list
  if (ios /= 0 .or. .not. external_list(1) .or. external_list(2)) error stop 8
  ios = 77
  read(unit, '(L6,1X,L7)', iostat=ios) external_fmt
  if (ios /= 0 .or. .not. external_fmt(1) .or. external_fmt(2)) error stop 9
  ios = 77
  read(unit, *, iostat=ios) value
  if (ios /= 0 .or. value%narrow .or. .not. value%ordinary) error stop 10
  if (value%guard /= 44) error stop 11
  close(unit)

  raw1 = .true.
  raw2 = .false.
  raw4 = .true.
  raw8 = .false.
  raw_guard1 = 11
  raw_guard2 = 22
  raw_guard4 = 44
  raw_guard8 = 88
  open(newunit=unit, status='scratch', form='unformatted', action='readwrite')
  write(unit) raw1, raw_guard1, raw2, raw_guard2, raw4, raw_guard4, raw8, raw_guard8
  rewind(unit)
  raw1 = .false.
  raw2 = .true.
  raw4 = .false.
  raw8 = .true.
  raw_guard1 = 0
  raw_guard2 = 0
  raw_guard4 = 0
  raw_guard8 = 0
  ios = 77
  read(unit, iostat=ios) raw1, raw_guard1, raw2, raw_guard2, raw4, raw_guard4, raw8, raw_guard8
  if (ios /= 0 .or. .not. raw1 .or. raw2 .or. .not. raw4 .or. raw8) error stop 12
  if (raw_guard1 /= 11 .or. raw_guard2 /= 22 .or. raw_guard4 /= 44 .or. &
      raw_guard8 /= 88) error stop 13
  close(unit)

  value%narrow = .true.
  value%guard = 55
  value%ordinary = .false.
  open(newunit=unit, status='scratch', form='unformatted', action='readwrite')
  write(unit) value
  rewind(unit)
  value%narrow = .false.
  value%guard = 0
  value%ordinary = .true.
  ios = 77
  read(unit, iostat=ios) value
  if (ios /= 0 .or. .not. value%narrow .or. value%ordinary) error stop 14
  if (value%guard /= 55) error stop 15
  close(unit)

  print *, 'ok'
end program read_logical_items
