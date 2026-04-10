module integer16_globals_backend
  implicit none
  integer(16), save :: big_scalar = 18446744073709551616_16
  integer(16), save :: big_array(2) = [1_16, 9223372036854775808_16]
contains
  subroutine touch()
  end subroutine touch
end module integer16_globals_backend
