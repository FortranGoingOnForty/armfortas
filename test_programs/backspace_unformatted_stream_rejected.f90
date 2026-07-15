! CHECK: ok
program backspace_unformatted_stream_rejected
  use, intrinsic :: iso_fortran_env, only : int8, int32
  implicit none

  character(len=64) :: message
  integer :: unit, ios
  integer(kind=8) :: before, after
  integer(int8) :: byte
  integer(int32) :: marker_like_payload(3)

  open(newunit=unit, status='scratch', access='stream', form='unformatted', &
       action='readwrite')
  marker_like_payload = [4_int32, 42_int32, 4_int32]
  write(unit) marker_like_payload
  inquire(unit=unit, pos=before)
  ios = 77
  message = 'sentinel'
  backspace(unit=unit, iostat=ios, iomsg=message, err=100)
  error stop 1
100 continue
  if (ios == 0) error stop 2
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 3
  inquire(unit=unit, pos=after)
  if (after /= before) error stop 4
  byte = 0_int8
  read(unit, pos=before, iostat=ios) byte
  if (ios >= 0) error stop 5
  close(unit)

  open(newunit=unit, status='scratch', access='stream', form='formatted', &
       action='readwrite')
  write(unit, '(A)') 'line'
  inquire(unit=unit, pos=before)
  ios = 77
  message = 'sentinel'
  backspace(unit=unit, iostat=ios, iomsg=message, err=200)
  error stop 6
200 continue
  if (ios == 0) error stop 7
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 8
  inquire(unit=unit, pos=after)
  if (after /= before) error stop 9
  close(unit)

  print *, 'ok'
end program backspace_unformatted_stream_rejected
