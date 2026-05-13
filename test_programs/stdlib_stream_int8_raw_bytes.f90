! CHECK: ok
! IR_CHECK: call @afs_write_int8(
! IR_CHECK: call @afs_read_int8(
! REPRO_CHECK: run_same_sandbox
program stdlib_stream_int8_raw_bytes
  use iso_fortran_env, only: int8
  implicit none

  integer :: u, ios, file_size
  integer(int8) :: bytes(4), got(4)

  bytes = [50_int8, 9_int8, 46_int8, -34_int8]
  got = 0_int8

  open(newunit=u, file='stdlib_stream_int8_raw_bytes.bin', status='replace', &
       action='write', access='stream', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 1
  write(u, iostat=ios) bytes
  if (ios /= 0) error stop 2
  close(u)

  inquire(file='stdlib_stream_int8_raw_bytes.bin', size=file_size)
  if (file_size /= 4) error stop 3

  open(newunit=u, file='stdlib_stream_int8_raw_bytes.bin', status='old', &
       action='read', access='stream', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 4
  read(u, iostat=ios) got
  if (ios /= 0) error stop 5
  close(u, status='delete')

  if (got(1) /= 50_int8) error stop 6
  if (got(2) /= 9_int8) error stop 7
  if (got(3) /= 46_int8) error stop 8
  if (got(4) /= -34_int8) error stop 9
  print *, 'ok'
end program
