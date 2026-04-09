! Module-level array variables. Before the fix, collect_module_globals
! discarded entity.array_spec and entity.init; arr(N) became a single
! i32 global, initializers were dropped, element access linker-
! failed because the program unit never saw the array's base.
! Audit BLOCKING-2.
!
! CHECK: 10 20 30
! CHECK: 0 0 0 0
! CHECK: 1.5
! CHECK: 2.5
! CHECK: 99
module mod_arr
  integer :: arr(3) = [10, 20, 30]
  integer :: zeros(4)
  real :: reals(2) = [1.5, 2.5]
end module mod_arr

program p
  use mod_arr
  print *, arr(1), arr(2), arr(3)
  print *, zeros(1), zeros(2), zeros(3), zeros(4)
  print *, reals(1)
  print *, reals(2)
  arr(1) = 99
  print *, arr(1)
end program
