! CHECK: ok
! REPRO_CHECK: run
program p
  implicit none
  integer :: unit_num, ios
  character(len=10) :: text
  character :: extra

  open(newunit=unit_num, file='stream_default_unformatted_write.dat', &
       access='stream', action='write', position='rewind', iostat=ios)
  if (ios /= 0) error stop 1
  write(unit_num, iostat=ios) 'test input'
  if (ios /= 0) error stop 2
  close(unit_num)

  open(newunit=unit_num, file='stream_default_unformatted_write.dat', &
       access='stream', action='read', status='old', iostat=ios)
  if (ios /= 0) error stop 3
  read(unit_num, iostat=ios) text
  if (ios /= 0) error stop 4
  if (text /= 'test input') error stop 5
  read(unit_num, iostat=ios) extra
  if (ios >= 0) error stop 6
  close(unit_num, status='delete')

  print *, 'ok'
end program p
