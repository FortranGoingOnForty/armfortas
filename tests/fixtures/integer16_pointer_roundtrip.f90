program integer16_pointer_roundtrip
  use iso_c_binding, only: c_ptr, c_loc, c_f_pointer
  implicit none

  integer(16), target :: wide
  integer(16), pointer :: view
  type(c_ptr) :: raw

  wide = 42_16
  raw = c_loc(wide)
  call c_f_pointer(raw, view)

  if (.not. associated(view)) error stop 1
  if (view /= 42_16) error stop 2
  print *, "ok"
end program integer16_pointer_roundtrip
