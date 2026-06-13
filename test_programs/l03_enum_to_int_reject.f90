! l03: no implicit conversion from an enumeration value to INTEGER —
! the ordinal must be extracted with INT(v).
! FLAGS: --std=f2023
! ERROR_EXPECTED: convert with INT(v)
program l03_enum_to_int_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  integer :: n
  c = red
  n = c
  print *, n
end program l03_enum_to_int_reject
