! CHECK: ok
program stream_pos_resets_read_state
  implicit none

  character(len=*), parameter :: list_path = 'stream_pos_list.tmp'
  character(len=*), parameter :: format_path = 'stream_pos_format.tmp'
  character(len=1) :: first_char, second_char
  integer :: unit, ios, first, second

  open(newunit=unit, file=list_path, status='replace', access='stream', &
       form='formatted', action='readwrite')
  write(unit, '(A)') '1 2'
  first = 9
  second = 9
  read(unit=unit, fmt=*, pos=1, iostat=ios) first
  if (ios /= 0) error stop 1
  read(unit=unit, fmt=*, pos=1, iostat=ios) second
  if (ios /= 0 .or. first /= 1 .or. second /= 1) error stop 2
  close(unit, status='delete')

  open(newunit=unit, file=format_path, status='replace', access='stream', &
       form='formatted', action='readwrite')
  write(unit, '(A)') 'AB'
  first_char = '?'
  second_char = '?'
  read(unit=unit, fmt='(A1)', pos=1, advance='no', iostat=ios) first_char
  if (ios /= 0) error stop 3
  read(unit=unit, fmt='(A1)', pos=1, advance='no', iostat=ios) second_char
  if (ios /= 0 .or. first_char /= 'A' .or. second_char /= 'A') error stop 4
  close(unit, status='delete')

  print *, 'ok'
end program stream_pos_resets_read_state
