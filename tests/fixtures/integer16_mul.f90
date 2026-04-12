program integer16_mul
  implicit none
  integer(16) :: x

  x = 6_16 * 7_16
  ! Keep x live so the store and the folded constant survive DCE —
  ! the O1 const-fold test wants to observe the folded value in the
  ! emitted asm, not just the absence of an imul.  The O0 rejection
  ! test still fires before the print is considered because the imul
  ! is what triggers the i128-codegen-not-yet-supported gate.
  print *, x
end program integer16_mul
