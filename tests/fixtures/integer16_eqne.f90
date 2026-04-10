program integer16_eqne
  implicit none
  integer(16) :: x
  integer(16) :: y
  integer(16) :: z
  logical :: eq
  logical :: ne

  x = 42_16
  y = 42_16
  z = 7_16
  eq = x == y
  ne = x /= z
end program integer16_eqne
