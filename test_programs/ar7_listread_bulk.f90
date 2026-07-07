! CHECK: checksum=5000050000
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar7_listread_bulk
  implicit none
  integer, parameter :: n = 100000
  integer, allocatable :: a(:), b(:)
  integer :: i, ios, u
  integer(kind=8) :: checksum, expected

  allocate(a(n), b(n))
  do i = 1, n
    a(i) = i
  end do
  b = -1

  open(newunit=u, status='scratch', action='readwrite', form='formatted', iostat=ios)
  if (ios /= 0) error stop 1

  write(u, *, iostat=ios) a
  if (ios /= 0) error stop 2

  rewind(u)
  read(u, *, iostat=ios) b
  if (ios /= 0) error stop 3

  checksum = 0_8
  do i = 1, n
    if (b(i) /= i) error stop 4
    checksum = checksum + int(b(i), kind=8)
  end do

  expected = int(n, kind=8) * int(n + 1, kind=8) / 2_8
  print '(a,i0)', 'checksum=', checksum
  if (checksum /= expected) error stop 5

  close(u)
  print '(a)', 'ok'
end program
