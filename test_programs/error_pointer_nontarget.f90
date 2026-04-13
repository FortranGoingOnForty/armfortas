! Pointer assignment source must have TARGET or POINTER attribute.
! ERROR_EXPECTED: must have target or pointer attribute
program t
  implicit none
  integer, pointer :: ptr
  integer :: plain = 42
  ptr => plain
end program
