! l01: RANK(n>0) on a plain local (not a dummy, not allocatable or
! pointer) violates the F2023 constraint and must error.
! FLAGS: --std=f2023
! ERROR_EXPECTED: must be a dummy argument or have ALLOCATABLE or POINTER
program l01_rank_attr_misuse
  implicit none
  integer, rank(2) :: a
  print *, 0
end program l01_rank_attr_misuse
