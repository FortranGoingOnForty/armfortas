! l03: enumeration types are a distinct TKR — no implicit conversion
! from INTEGER. A bare integer on the right of an enumeration
! assignment must go through the constructor.
! FLAGS: --std=f2023
! ERROR_EXPECTED: use the constructor 'color(int-expr)'
program l03_enum_assign_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = 1
  print *, 'unreachable'
end program l03_enum_assign_reject
