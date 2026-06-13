! l03: values of one enumeration type do not convert to another —
! the enumeration NAME is part of the type.
! FLAGS: --std=f2023
! ERROR_EXPECTED: cannot assign a value of enumeration type 'color' to a variable of enumeration type 'fruit'
program l03_enum_assign_cross_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  enumeration type :: fruit
    enumerator :: apple, pear
  end enumeration type
  type(fruit) :: f
  f = red
  print *, 'unreachable'
end program l03_enum_assign_cross_reject
