! NULLIFY target must have pointer attribute.
! ERROR_EXPECTED: must have pointer attribute
program t
  implicit none
  integer :: x = 42
  nullify(x)
end program
