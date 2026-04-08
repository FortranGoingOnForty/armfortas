! Audit #4 CRITICAL-2 — `integer(kind=1)` initializers and loads
! now respect the declared 8-bit width.
!
! Two-part fix:
!
! 1. eval_const_global_init clamps/sign-extends the folded
!    value against the declared target type before producing
!    the GlobalInit. So `integer(kind=1) :: y = 128` clamps to
!    -128 (the low 8 bits sign-extended), and `= 256` clamps
!    to 0. Previously the unclamped 256 reached `.byte 256` and
!    the assembler errored "out of range literal value".
!
! 2. isel's Load arm picks the load opcode by inst.ty width.
!    i8 → LDRSB (load signed byte), i16 → LDRSH, i32/Bool → LDR.
!    Previously every load used 32-bit `ldr w_, [_]` which read
!    4 bytes from a 1-byte slot — adjacent globals corrupted
!    each other (127 followed by 128 read back as 32895).
!
! 3. isel's Store arm picks STRB / STRH / STR by the pointer's
!    pointee type, so an i8 store is a single-byte write that
!    doesn't bleed into the next slot.
!
! CHECK: 127
! CHECK: -128
program audit4_c2_kind1_overflow
  integer(kind=1) :: x = 127
  integer(kind=1) :: y = 128
  print *, x
  print *, y
end program
