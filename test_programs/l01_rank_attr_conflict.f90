! l01: RANK and DIMENSION are mutually exclusive.
! FLAGS: --std=f2023
! ERROR_EXPECTED: RANK and DIMENSION cannot both be specified
program l01_rank_attr_conflict
  implicit none
  integer, rank(2), dimension(2, 3), allocatable :: a
  print *, 0
end program l01_rank_attr_conflict
