! NULLIFY on a plain (non-pointer) array variable.
! ERROR_EXPECTED: must have pointer attribute
program t
  implicit none
  integer :: arr(10)
  nullify(arr)
end program
