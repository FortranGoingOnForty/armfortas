! CHECK: ok
! FILE_CHECK: scratch_file_victim.txt => keep
! FILE_MISSING: scratch_file_missing.txt
! FILE_SET_EXACT: scratch_file_victim.txt
! REPRO_CHECK: run
program p
  implicit none
  character(len=*), parameter :: payload = 'keep-this-file'
  integer :: unit_num, ios
  integer(kind=8) :: file_size
  logical :: exists, opened
  character(len=len(payload)) :: content
  character(len=128) :: message

  open(newunit=unit_num, file='scratch_file_victim.txt', &
       status='replace', access='stream', form='unformatted', action='write')
  write(unit_num) payload
  close(unit_num)

  message = ''
  open(unit=83, file='scratch_file_victim.txt', &
       status='scratch', iostat=ios, iomsg=message)
  if (ios == 0) then
    close(83)
    error stop 1
  end if
  if (len_trim(message) == 0) error stop 2
  inquire(unit=83, opened=opened)
  if (opened) error stop 3

  inquire(file='scratch_file_victim.txt', exist=exists, size=file_size)
  if (.not. exists) error stop 4
  if (file_size /= len(payload)) error stop 5
  open(newunit=unit_num, file='scratch_file_victim.txt', &
       status='old', access='stream', form='unformatted', action='read', iostat=ios)
  if (ios /= 0) error stop 6
  read(unit_num, iostat=ios) content
  close(unit_num)
  if (ios /= 0) error stop 7
  if (content /= payload) error stop 8

  message = ''
  open(unit=84, file='scratch_file_missing.txt', &
       status='scratch', iostat=ios, iomsg=message)
  if (ios == 0) then
    close(84)
    error stop 9
  end if
  inquire(file='scratch_file_missing.txt', exist=exists)
  if (exists) error stop 10

  print *, 'ok'
end program p
