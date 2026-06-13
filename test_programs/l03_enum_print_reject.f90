! l03: list-directed output of an enumeration value is invalid (the
! F2023 7.6.2 NOTE: "list-directed i/o not available" for enumeration
! types); the diagnostic points at INT(v).
! FLAGS: --std=f2023
! ERROR_EXPECTED: list-directed output of enumeration type 'color' is not allowed
program l03_enum_print_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = red
  print *, c
end program l03_enum_print_reject
