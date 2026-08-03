! ERROR_EXPECTED: INQUIRE(IOLENGTH=) result must be a definable scalar INTEGER variable
program error_inquire_iolength_result
  implicit none

  character :: result

  inquire(iolength=result) 1
end program error_inquire_iolength_result
