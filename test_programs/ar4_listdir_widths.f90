program ar4_listdir_widths
  use iso_fortran_env, only: int8, int16, int32, int64
  implicit none

  integer(int8) :: i1 = 0_int8
  integer(int16) :: i2 = 0_int16
  integer(int32) :: i4 = 0_int32
  integer(int64) :: i8 = 0_int64
  integer :: unit
  integer :: bytes
  integer :: ios
  character(len=80) :: line
  character(len=45) :: raw
  character(len=*), parameter :: path = 'ar4_listdir_widths.out'

  write(line, *) i1
  call show('i1', line)
  ! CHECK: i1 5 [    0]

  write(line, *) i2
  call show('i2', line)
  ! CHECK: i2 7 [      0]

  write(line, *) i4
  call show('i4', line)
  ! CHECK: i4 12 [           0]

  write(line, *) i8
  call show('i8', line)
  ! CHECK: i8 21 [                    0]

  write(line, *) i1, i2, i4, i8
  call show('all', line)
  ! CHECK: all 45 [    0      0           0                    0]

  open(newunit=unit, file=path, status='replace', action='write', form='formatted')
  write(unit, *) i1, i2, i4, i8
  close(unit)

  inquire(file=path, size=bytes)
  print '(a,1x,i0)', 'external_bytes', bytes
  ! CHECK: external_bytes 46

  raw = repeat('.', len(raw))
  open(newunit=unit, file=path, status='old', action='read', access='stream', form='unformatted')
  read(unit, pos=1, iostat=ios) raw
  close(unit, status='delete')

  print '(a,1x,i0)', 'external_ios', ios
  ! CHECK: external_ios 0
  print '(a,1x,i0,1x,"[",a,"]")', 'external', len(raw), raw
  ! CHECK: external 45 [    0      0           0                    0]

contains
  subroutine show(label, s)
    character(len=*), intent(in) :: label
    character(len=*), intent(in) :: s
    integer :: n

    n = len_trim(s)
    print '(a,1x,i0,1x,"[",a,"]")', label, n, s(:n)
  end subroutine
end program
