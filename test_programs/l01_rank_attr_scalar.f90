! l01: RANK(0) declares a scalar (F2023 8.5.17).
! FLAGS: --std=f2023
! CHECK: 7
program l01_rank_attr_scalar
  implicit none
  integer, rank(0) :: s
  s = 7
  print *, s
end program l01_rank_attr_scalar
