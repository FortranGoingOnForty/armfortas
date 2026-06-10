! l01: RANK(n) is F2023; older --std levels reject it.
! FLAGS: --std=f2018
! ERROR_EXPECTED: RANK(n) attribute requires --std=F2023
program l01_rank_attr_std
  implicit none
  integer, rank(1), allocatable :: a
  print *, 0
end program l01_rank_attr_std
