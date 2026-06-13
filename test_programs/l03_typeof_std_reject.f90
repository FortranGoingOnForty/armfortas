! l03: TYPEOF is F2023; older std levels reject the declaration.
! FLAGS: --std=f2018
! ERROR_EXPECTED: TYPEOF requires --std=F2023
program l03_typeof_std_reject
  implicit none
  integer :: n
  typeof(n) :: m
  n = 1
  m = n
  print *, m
end program l03_typeof_std_reject
