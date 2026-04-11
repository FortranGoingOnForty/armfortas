program integer16_internal_format_read_alloc_reverse_section
  implicit none
  character(len=16) :: buf
  integer(16), allocatable :: a(:,:)

  allocate(a(0:1, 2:2))
  a = 0_16
  buf = ' 7  8'
  read(buf, '(I2,1X,I2)') a(1:0:-1, 2)

  print *, a(0,2)
end program integer16_internal_format_read_alloc_reverse_section
