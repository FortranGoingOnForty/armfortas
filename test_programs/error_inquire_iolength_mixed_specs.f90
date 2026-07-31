! ERROR_EXPECTED: INQUIRE(IOLENGTH=) may not be combined with other specifiers
program error_inquire_iolength_mixed_specs
  implicit none

  integer :: result

  inquire(iolength=result, unit=6) 1
end program error_inquire_iolength_mixed_specs
