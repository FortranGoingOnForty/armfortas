! l03: ENUMERATION TYPE is an F2023 feature; older std levels reject
! the definition itself.
! FLAGS: --std=f2018
! ERROR_EXPECTED: ENUMERATION TYPE requires --std=F2023
program l03_enumeration_std_reject
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  print *, 'unreachable'
end program l03_enumeration_std_reject
