! CHECK: [o][Hello]
! IR_CHECK: call @afs_seek_stream
! REPRO_CHECK: run
program p
  implicit none
  integer :: unit_num, ios
  character :: tail
  character(len=5) :: text

  open(newunit=unit_num, file='stream_pos_rewind_after_probe.dat', status='replace', &
       access='stream', form='unformatted', action='write')
  write(unit_num) 'Hello'
  close(unit_num)

  open(newunit=unit_num, file='stream_pos_rewind_after_probe.dat', status='old', &
       access='stream', form='unformatted', action='read')
  read(unit_num, pos=5, iostat=ios) tail
  if (ios /= 0) error stop 1
  read(unit_num, pos=1, iostat=ios) text
  if (ios /= 0) error stop 2
  close(unit_num, status='delete')

  print *, '[' // tail // '][' // text // ']'
end program p
