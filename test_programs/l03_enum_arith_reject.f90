! l03: arithmetic is not defined for enumeration types (F2023 7.6.2 —
! NEXT/PREVIOUS are the standard's increment mechanism, not +).
! FLAGS: --std=f2023
! ERROR_EXPECTED: operator '+' is not defined for enumeration type 'color'
program l03_enum_arith_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = red
  c = c + 1
  print *, 'unreachable'
end program l03_enum_arith_reject
