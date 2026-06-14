! l02a item 1: ALLOCATE bounds from an array constructor (F2023 R937).
! `allocate(x([2,3]))` allocates the same 2x3 array as `allocate(x(2,3))`;
! each constructor element supplies one dimension's upper bound (lower 1).
! Rejected loudly until the vector-bounds lowering landed.
! FLAGS: --std=f2023
program l02a_allocate_array_bounds
  implicit none
  integer, allocatable :: x(:, :)
  allocate(x([2, 3]))
  x = 7
  print '(I0,1X,I0)', shape(x)
  ! CHECK: 2 3
  print '(I0)', sum(x)
  ! CHECK: 42
  deallocate(x)
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l02a_allocate_array_bounds
