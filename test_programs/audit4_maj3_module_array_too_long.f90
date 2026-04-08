! Audit #4 MAJOR-3 — module array with too-long initializer
! must be a compile-time error.
!
! Per F2018 §7.4.4, an initializer's shape must conform with the
! variable's declared shape. `integer :: arr(2) = [1,2,3]` is a
! constraint violation. Today eval_const_array_init silently
! truncates and emit_globals doesn't assert the count matches.
!
! When the fix lands, the compiler must reject this with a
! diagnostic mentioning the shape mismatch. Stderr should
! contain "shape" or "initializer" or similar.
!
! XFAIL: audit MAJOR-3 (module array too-long initializer not diagnosed)
! ERROR_EXPECTED: shape
program audit4_maj3_module_array_too_long
  use audit4_maj3_mod
  print *, arr(1)
end program

module audit4_maj3_mod
  integer :: arr(2) = [1, 2, 3]   ! 3 elements into a 2-slot array
end module audit4_maj3_mod
