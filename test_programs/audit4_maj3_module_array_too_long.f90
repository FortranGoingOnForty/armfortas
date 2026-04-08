! Audit #4 MAJOR-3 — module array with too-long initializer is
! now diagnosed as a compile-time error.
!
! Fixed: collect_module_globals invokes collect_const_array_scalars
! BEFORE eval_const_array_init and checks scalars.len() against
! the declared total. Over-long is rejected with an error
! mentioning the actual vs expected element counts.
!
! Per F2018 §7.4.4 the initializer's shape must conform with the
! variable's declared shape; this used to be silently truncated.
!
! ERROR_EXPECTED: shape
program audit4_maj3_module_array_too_long
  use audit4_maj3_mod
  print *, arr(1)
end program

module audit4_maj3_mod
  integer :: arr(2) = [1, 2, 3]   ! 3 elements into a 2-slot array
end module audit4_maj3_mod
