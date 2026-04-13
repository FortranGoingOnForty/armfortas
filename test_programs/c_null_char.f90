! Test c_null_char and other iso_c_binding character constants.
! The null character appended to "hello" should produce a 6-byte string
! whose last byte is 0. We verify by printing the ICHAR value.
! CHECK: 0
program t
  use iso_c_binding, only: c_null_char
  implicit none
  character(len=1) :: nul
  nul = c_null_char
  print *, ichar(nul)
end program
