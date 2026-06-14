! l02a item 3: array-valued F2023 conditional expression as an assignment
! RHS. Lowered as a per-arm branch (lower_array_conditional_assign), so it
! reuses the ordinary assignment path: fixed-size, allocatable auto-realloc,
! sections, constructors, and scalar-broadcast arms all work.
! FLAGS: --std=f2023
program l02a_array_conditional
  implicit none
  integer :: a(3), b(3), x(3)
  integer, allocatable :: y(:)
  logical :: c
  a = [1, 2, 3]
  b = [10, 20, 30]

  ! whole-array arms into a fixed-size dest
  c = .true.
  x = (c ? a : b)
  print '(3(I0,1X))', x
  ! CHECK: 1 2 3
  c = .false.
  x = (c ? a : b)
  print '(3(I0,1X))', x
  ! CHECK: 10 20 30

  ! scalar-broadcast arms
  x = (a(1) > 0 ? 7 : -7)
  print '(3(I0,1X))', x
  ! CHECK: 7 7 7

  ! array constructor arm + section arm into allocatable (auto-realloc)
  y = (c ? [100, 200] : b(2:3))
  print '(2(I0,1X))', y
  ! CHECK: 20 30
  c = .true.
  y = (c ? [100, 200] : b(2:3))
  print '(2(I0,1X))', y
  ! CHECK: 100 200

  ! chained conditional
  x = (a(1) > 5 ? a : (b(1) > 5 ? b : a))
  print '(3(I0,1X))', x
  ! CHECK: 10 20 30
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l02a_array_conditional
