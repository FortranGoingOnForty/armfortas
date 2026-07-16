! CHECK: ok
program read_list_null_items
  implicit none

  character(len=64) :: source
  character(len=8) :: char_first, char_second
  integer :: int_first, int_second, ios, unit
  integer :: values(3)
  integer(1) :: byte_first, byte_second
  integer(2) :: short_first, short_second
  real :: real_first, real_second
  logical :: logical_first, logical_second

  source = ',42,,2.5,,F,,word'
  int_first = 7
  int_second = 8
  real_first = -7.0
  real_second = -8.0
  logical_first = .true.
  logical_second = .true.
  char_first = 'keep'
  char_second = 'old'
  ios = 77
  read(source, *, iostat=ios) int_first, int_second, real_first, real_second, &
                               logical_first, logical_second, char_first, char_second
  if (ios /= 0) error stop 1
  if (int_first /= 7 .or. int_second /= 42) error stop 2
  if (real_first /= -7.0 .or. abs(real_second - 2.5) > 1.0e-6) error stop 3
  if (.not. logical_first .or. logical_second) error stop 4
  if (char_first /= 'keep' .or. trim(char_second) /= 'word') error stop 5

  source = ',12,,30000'
  byte_first = 7
  byte_second = 8
  short_first = 70
  short_second = 80
  ios = 77
  read(source, *, iostat=ios) byte_first, byte_second, short_first, short_second
  if (ios /= 0) error stop 6
  if (byte_first /= 7 .or. byte_second /= 12) error stop 7
  if (short_first /= 70 .or. short_second /= 30000) error stop 8

  source = '1, ,3'
  values = [7, 8, 9]
  ios = 77
  read(source, *, iostat=ios) values
  if (ios /= 0 .or. any(values /= [1, 8, 3])) error stop 9

  open(newunit=unit, status='scratch', action='readwrite')
  write(unit, '(A)') ',42,,2.5,,F,,word'
  rewind(unit)
  int_first = 7
  int_second = 8
  real_first = -7.0
  real_second = -8.0
  logical_first = .true.
  logical_second = .true.
  char_first = 'keep'
  char_second = 'old'
  ios = 77
  read(unit, *, iostat=ios) int_first, int_second, real_first, real_second, &
                             logical_first, logical_second, char_first, char_second
  if (ios /= 0) error stop 10
  if (int_first /= 7 .or. int_second /= 42) error stop 11
  if (real_first /= -7.0 .or. abs(real_second - 2.5) > 1.0e-6) error stop 12
  if (.not. logical_first .or. logical_second) error stop 13
  if (char_first /= 'keep' .or. trim(char_second) /= 'word') error stop 14
  close(unit)

  source = ',42'
  int_first = 7
  int_second = 8
  read(source, *) int_first, int_second
  if (int_first /= 7 .or. int_second /= 42) error stop 15

  print *, 'ok'
end program read_list_null_items
