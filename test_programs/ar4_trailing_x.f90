program ar4_trailing_x
  implicit none

  integer :: unit
  integer :: bytes
  integer :: ios
  integer :: i
  character(len=8) :: raw
  character(len=*), parameter :: path = 'ar4_trailing_x.out'

  open(newunit=unit, file=path, status='replace', action='write', form='formatted')
  write(unit, '(i0,1x)') 5
  write(unit, '(3(i0,1x))') 1, 2, 3
  close(unit)

  inquire(file=path, size=bytes)
  print *, 'bytes', bytes
  ! CHECK: bytes 8

  raw = '........'
  open(newunit=unit, file=path, status='old', action='read', access='stream', form='unformatted')
  read(unit, pos=1, iostat=ios) raw
  close(unit, status='delete')

  print *, 'readios', ios
  ! CHECK: readios 0
  print '(a,8(1x,i0))', 'codes', (iachar(raw(i:i)), i = 1, 8)
  ! CHECK: codes 53 10 49 32 50 32 51 10
end program
