module integer16_wide_values_mod
  implicit none
  integer(16), save :: big_global = 9223372036854775808_16
end module integer16_wide_values_mod

program integer16_wide_values
  use integer16_wide_values_mod
  implicit none
  integer(16) :: big_local

  big_local = 170141183460469231731687303715884105727_16
end program integer16_wide_values
