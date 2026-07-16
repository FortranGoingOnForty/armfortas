! CHECK: ok
program read_formatted_real_state
  implicit none

  character(len=32) :: source
  integer :: ios, unit
  real :: single
  real(8) :: first, second

  source = '00123'
  first = -1.0d0
  ios = 77
  read(source, '(F5.2)', iostat=ios) first
  if (ios /= 0 .or. abs(first - 1.23d0) > 1.0d-12) error stop 1

  first = -1.0d0
  ios = 77
  read(source, '(1P,F5.2)', iostat=ios) first
  if (ios /= 0 .or. abs(first - 0.123d0) > 1.0d-12) error stop 2

  first = -1.0d0
  ios = 77
  read(source, '(-1P,F5.2)', iostat=ios) first
  if (ios /= 0 .or. abs(first - 12.3d0) > 1.0d-12) error stop 3

  source = '1.23E2'
  first = -1.0d0
  ios = 77
  read(source, '(1P,E6.2)', iostat=ios) first
  if (ios /= 0 .or. abs(first - 123.0d0) > 1.0d-12) error stop 4

  source = '1.23'
  first = -1.0d0
  ios = 77
  read(source, '(1P,F4.2)', iostat=ios) first
  if (ios /= 0 .or. abs(first - 0.123d0) > 1.0d-12) error stop 5

  source = '00123E2'
  first = -1.0d0
  ios = 77
  read(source, '(1P,E7.2)', iostat=ios) first
  if (ios /= 0 .or. abs(first - 123.0d0) > 1.0d-12) error stop 6

  source = '1 21 2'
  first = -1.0d0
  second = -1.0d0
  ios = 77
  read(source, '(BN,F3.0,BZ,F3.0)', iostat=ios) first, second
  if (ios /= 0 .or. first /= 12.0d0 .or. second /= 102.0d0) error stop 7

  source = '1,251.25'
  first = -1.0d0
  second = -1.0d0
  ios = 77
  read(source, '(DC,F4.2,DP,F4.2)', iostat=ios) first, second
  if (ios /= 0 .or. first /= 1.25d0 .or. second /= 1.25d0) error stop 8

  source = '0012300123'
  first = -1.0d0
  second = -1.0d0
  ios = 77
  read(source, '(2(1P,F5.2))', iostat=ios) first, second
  if (ios /= 0) error stop 9
  if (abs(first - 0.123d0) > 1.0d-12 .or. &
      abs(second - 0.123d0) > 1.0d-12) error stop 10

  open(newunit=unit, status='scratch', action='readwrite')
  write(unit, '(A)') '001231,25'
  rewind(unit)
  single = -1.0
  second = -1.0d0
  ios = 77
  read(unit, '(1P,F5.2,0P,DC,F4.2)', iostat=ios) single, second
  if (ios /= 0) error stop 11
  if (abs(single - 0.123) > 1.0e-6 .or. second /= 1.25d0) error stop 12
  close(unit)

  print *, 'ok'
end program read_formatted_real_state
