! c_f_pointer round-trip: c_loc to get C pointer, c_f_pointer to recover.
! Verifies the c_loc -> c_f_pointer -> dereference chain works correctly.
! CHECK: fptr= 42
program test_c_f_pointer
  use iso_c_binding, only: c_ptr, c_loc, c_f_pointer
  implicit none

  integer, target :: x = 42
  integer, pointer :: fptr
  type(c_ptr) :: cptr

  cptr = c_loc(x)
  call c_f_pointer(cptr, fptr)
  print *, 'fptr=', fptr
end program
