! l03: relational operators require BOTH operands of the same
! enumeration type — comparing against a bare integer is an error.
! FLAGS: --std=f2023
! ERROR_EXPECTED: cannot compare enumeration type 'color' with a non-enumeration value
program l03_enum_cmp_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = red
  if (c == 1) print *, 'unreachable'
end program l03_enum_cmp_reject
