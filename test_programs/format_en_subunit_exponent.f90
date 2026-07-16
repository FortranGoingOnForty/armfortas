! CHECK: ok
! REPRO_CHECK: run
program format_en_subunit_exponent
  implicit none
  character(len=14) :: records(5)

  write(records, '(EN14.3)') 0.1d0, 0.01d0, 0.001d0, 0.0001d0, -0.01d0
  if (trim(adjustl(records(1))) /= '100.000E-03') error stop 1
  if (trim(adjustl(records(2))) /= '10.000E-03') error stop 2
  if (trim(adjustl(records(3))) /= '1.000E-03') error stop 3
  if (trim(adjustl(records(4))) /= '100.000E-06') error stop 4
  if (trim(adjustl(records(5))) /= '-10.000E-03') error stop 5

  print *, 'ok'
end program format_en_subunit_exponent
