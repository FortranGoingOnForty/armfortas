! l03: TYPEOF must reference a previously declared entity with a
! complete type — a forward or unknown name is a clean diagnostic,
! not an Unknown type that detonates in lowering.
! FLAGS: --std=f2023
! ERROR_EXPECTED: TYPEOF(zzz) references an undeclared entity
program l03_typeof_undeclared_reject
  implicit none
  typeof(zzz) :: m
  m = 1
  print *, m
end program l03_typeof_undeclared_reject
