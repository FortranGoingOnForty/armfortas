program integer16_internal_format_read_alloc_section
  implicit none
  character(len=16) :: buf
  integer(16), allocatable :: a(:,:)

  allocate(a(0:1, 2:2))
  a = 0_16
  buf = ' 11 22'
  read(buf, '(I3,1X,I3)') a(:, :)

  print *, a(0,2)
end program integer16_internal_format_read_alloc_section
