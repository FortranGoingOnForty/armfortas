! l03: the enumeration constructor (R771) takes a 1-based ordinal no
! greater than the enumerator count; a constant out of range is a
! compile-time error.
! FLAGS: --std=f2023
! ERROR_EXPECTED: value 4 is out of range for enumeration type 'color' (valid ordinals are 1..3)
! ERROR_SPAN: 13:13
program l03_enum_ctor_range_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  type(color) :: c
  c = color(4)
  print *, 'unreachable'
end program l03_enum_ctor_range_reject
