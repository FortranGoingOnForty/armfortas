! CHECK: char_bytes_ok
! XFAIL(macos): Darwin rejects raw 0xe9 filename bytes before runtime I/O starts
! REPRO_CHECK: run_same_sandbox
program ar4_char_bytes
  implicit none

  integer :: u, ios, file_size
  logical :: good_exists, bad_exists
  character(len=32) :: good_name, bad_name, inquired_name
  character(len=3) :: payload, got

  good_name = 'ar4_byte_' // achar(233) // '.dat'
  bad_name = 'ar4_byte_' // achar(239) // achar(191) // achar(189) // '.dat'
  payload = achar(65) // achar(233) // achar(66)
  got = '---'

  call delete_if_present(good_name)
  call delete_if_present(bad_name)

  open(newunit=u, file=good_name, status='replace', action='readwrite', &
       form='formatted', iostat=ios)
  if (ios /= 0) error stop 1
  write(u, '(a)', iostat=ios) payload
  if (ios /= 0) error stop 2
  rewind(u)
  read(u, '(a)', iostat=ios) got
  if (ios /= 0) error stop 3

  if (iachar(got(1:1)) /= 65) error stop 4
  if (iachar(got(2:2)) /= 233) error stop 5
  if (iachar(got(3:3)) /= 66) error stop 6

  inquire(file=good_name, exist=good_exists, size=file_size, name=inquired_name)
  if (.not. good_exists) error stop 7
  if (file_size /= 4) error stop 8
  if (iachar(inquired_name(10:10)) /= 233) error stop 9

  inquire(file=bad_name, exist=bad_exists)
  if (bad_exists) error stop 10

  close(u, status='delete', iostat=ios)
  if (ios /= 0) error stop 11
  print '(a)', 'char_bytes_ok'

contains

  subroutine delete_if_present(name)
    character(len=*), intent(in) :: name
    integer :: tmp, stat

    open(newunit=tmp, file=name, status='old', iostat=stat)
    if (stat == 0) close(tmp, status='delete')
  end subroutine

end program
