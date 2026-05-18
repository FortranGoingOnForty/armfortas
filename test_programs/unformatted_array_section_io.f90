! CHECK: ok
! REPRO_CHECK: run
program unformatted_array_section_io
  implicit none

  type :: box_t
    integer(kind=8), allocatable :: blocks(:)
  end type

  integer :: unit_num, ios
  integer(kind=8), allocatable :: a(:), b(:)
  type(box_t) :: source_box, dest_box

  allocate(a(2), b(2), source_box%blocks(2), dest_box%blocks(2))
  a = [123456789_8, -77_8]
  b = -1_8
  source_box%blocks = [987654321_8, -33_8]
  dest_box%blocks = -1_8

  open(newunit=unit_num, form='unformatted', status='scratch', action='readwrite', iostat=ios)
  if (ios /= 0) error stop 1

  write(unit_num, iostat=ios) a(:)
  if (ios /= 0) error stop 2

  write(unit_num, iostat=ios) source_box%blocks(:)
  if (ios /= 0) error stop 3

  rewind(unit_num)

  read(unit_num, iostat=ios) b(:)
  if (ios /= 0) error stop 4

  read(unit_num, iostat=ios) dest_box%blocks(:)
  if (ios /= 0) error stop 5

  close(unit_num)

  if (any(b /= a)) error stop 6
  if (any(dest_box%blocks /= source_box%blocks)) error stop 7

  print *, 'ok'
end program
