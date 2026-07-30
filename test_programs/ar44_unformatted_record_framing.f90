! Sequential-unformatted input must reject truncated payloads, short
! trailers, and mismatched record markers before publishing any data.
!
! FLAGS: --std=f2023
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_list_read_begin
program ar44_unformatted_record_framing
  use iso_fortran_env, only: int16, int32
  implicit none

  integer :: unit
  integer :: ios
  integer(int16) :: short_marker
  integer(int32) :: marker
  integer(int32) :: payload

  marker = 8_int32
  payload = 1234_int32
  open(newunit=unit, file='ar44_truncated_payload.dat', status='replace', &
       action='write', access='stream', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 1
  write(unit, iostat=ios) marker, payload
  if (ios /= 0) error stop 2
  close(unit)

  payload = -1_int32
  open(newunit=unit, file='ar44_truncated_payload.dat', status='old', &
       action='read', access='sequential', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 3
  read(unit, iostat=ios) payload
  if (ios == 0) error stop 4
  close(unit, status='delete')

  marker = 4_int32
  payload = 2345_int32
  short_marker = 4_int16
  open(newunit=unit, file='ar44_truncated_trailer.dat', status='replace', &
       action='write', access='stream', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 5
  write(unit, iostat=ios) marker, payload, short_marker
  if (ios /= 0) error stop 6
  close(unit)

  payload = -1_int32
  open(newunit=unit, file='ar44_truncated_trailer.dat', status='old', &
       action='read', access='sequential', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 7
  read(unit, iostat=ios) payload
  if (ios == 0) error stop 8
  close(unit, status='delete')

  marker = 4_int32
  payload = 3456_int32
  open(newunit=unit, file='ar44_mismatched_trailer.dat', status='replace', &
       action='write', access='stream', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 9
  write(unit, iostat=ios) marker, payload, 8_int32
  if (ios /= 0) error stop 10
  close(unit)

  payload = -1_int32
  open(newunit=unit, file='ar44_mismatched_trailer.dat', status='old', &
       action='read', access='sequential', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 11
  read(unit, iostat=ios) payload
  if (ios == 0) error stop 12
  close(unit, status='delete')

  marker = 4_int32
  payload = 4567_int32
  open(newunit=unit, file='ar44_valid_record.dat', status='replace', &
       action='write', access='stream', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 13
  write(unit, iostat=ios) marker, payload, marker
  if (ios /= 0) error stop 14
  close(unit)

  payload = -1_int32
  open(newunit=unit, file='ar44_valid_record.dat', status='old', &
       action='read', access='sequential', form='unformatted', iostat=ios)
  if (ios /= 0) error stop 15
  read(unit, iostat=ios) payload
  if (ios /= 0) error stop 16
  if (payload /= 4567_int32) error stop 17
  close(unit, status='delete')

  print '(a)', 'ok'
end program ar44_unformatted_record_framing
