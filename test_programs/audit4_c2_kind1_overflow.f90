! Audit #4 CRITICAL-2 — `integer(kind=1) :: x = 256` either
! crashes assembly or silently corrupts the value.
!
! eval_const_global_init returns the i64 value as-is and emit_globals
! writes it via `.byte 256` which is out of range for an unsigned
! byte directive. Even in-range cases like 128 are wrong because the
! value is interpreted as signed at runtime: 128 → -128 in i8.
!
! The fix needs eval_const_scalar (or its caller) to clamp / sign-
! extend the value against the declared target width and reject
! constants that don't fit.
!
! Two assertions:
!  * 127 (max signed i8) should round-trip cleanly
!  * 128 should produce a compile-time diagnostic OR sign-extend to -128
!
! XFAIL: audit CRITICAL-2 (kind=1 init overflow)
! CHECK: 127
! CHECK: -128
program audit4_c2_kind1_overflow
  integer(kind=1) :: x = 127
  integer(kind=1) :: y = 128
  print *, x
  print *, y
end program
