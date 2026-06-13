! l03: the NAMED form of ENUM, BIND(C) is F2023 (R760); the unnamed
! F2003 form stays legal at older levels.
! FLAGS: --std=f2018
! ERROR_EXPECTED: named interoperable ENUM type requires --std=F2023
program l03_enum_bindc_named_std_reject
  implicit none
  enum, bind(c) :: speed
    enumerator :: slow = 10
  end enum
  print *, slow
end program l03_enum_bindc_named_std_reject
