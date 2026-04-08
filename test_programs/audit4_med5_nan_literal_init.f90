! Audit #4 MEDIUM-5 — NaN/Inf in a const-folded initializer
! emits non-portable assembly.
!
! emit_globals formats Float values via Rust's `Display`, which
! produces "NaN" / "inf" for non-finite IEEE values. Apple's `as`
! accepts `.single NaN` (which is why this test passes today).
! GNU binutils does NOT — `.single NaN` → "unknown constant".
!
! This program is **not** XFAIL'd because the runtime path works
! correctly on our current target (macOS / Apple's `as`). It
! pins the macOS-side regression: a parameter folded to NaN at
! compile time must round-trip through .data → load → print as
! NaN at runtime.
!
! The actual MEDIUM-5 fix is about emitting bit-pattern hex
! literals (`.long 0x7fc00000`, `.quad 0x7ff8000000000000`)
! instead of relying on the assembler's NaN parsing. That fix
! becomes load-bearing once we add a Linux/GNU-binutils target;
! until then this program documents the runtime expectation.
!
! CHECK: NaN
program audit4_med5_nan_literal_init
  real :: x = (-1.0) ** 0.5    ! NaN by IEEE
  print *, x
end program
