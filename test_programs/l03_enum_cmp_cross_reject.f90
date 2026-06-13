! l03: relational operators on two DIFFERENT enumeration types are an
! error even though both lower to integer ordinals.
! FLAGS: --std=f2023
! ERROR_EXPECTED: must have the same enumeration type ('color' vs 'fruit')
program l03_enum_cmp_cross_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  enumeration type :: fruit
    enumerator :: apple, pear
  end enumeration type
  if (red == apple) print *, 'unreachable'
end program l03_enum_cmp_cross_reject
