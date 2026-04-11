program integer16_format_read_alloc_section
  implicit none
  integer(16), allocatable :: a(:,:)

  allocate(a(0:1, 2:2))
  a = 0_16

  open(10, file='afs_fmt_read_i128_alloc_section.dat', status='replace', action='readwrite')
  write(10, '(A)') ' 11 22'
  rewind(10)
  read(10, '(I3,1X,I3)') a(:, :)
  close(10)

  print *, a(0,2)
end program integer16_format_read_alloc_section
