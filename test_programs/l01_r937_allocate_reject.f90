! l01: ALLOCATE bounds from an array expression (F2023 R937) used to
! compile into garbage extents. Until the vector-bounds lowering
! lands it must error loudly, never mis-allocate.
! FLAGS: --std=f2023
! ERROR_EXPECTED: ALLOCATE bounds from an array expression
program l01_r937_allocate_reject
  implicit none
  integer, allocatable :: x(:, :)
  allocate(x([2, 3]))
  print *, 0
end program l01_r937_allocate_reject
