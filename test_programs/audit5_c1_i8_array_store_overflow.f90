! Audit #5 CRITICAL-1 — i8 array store emits 4-byte STR through
! a 1-byte slot, clobbering the next 3 bytes.
!
! The audit-4 CRITICAL-2 fix made isel pick the store opcode by
! the IR VALUE's type (a deliberate choice to support byte-level
! GEPs into derived types where Ptr<i8> is a generic offset
! cursor). The dual obligation is that lower_array_store must
! truncate the value to the element type before the store —
! and it doesn't. The verifier was supposed to catch this via
! the audit-4 Maj-2 check, but `pointee_is_byte` escapes the
! check for every i8 store, including legitimate array stores.
!
! Test design: the canary is passed BY REFERENCE to a subroutine,
! which forces mem2reg to keep it as a memory alloca rather than
! promoting it to a register. Without this, mem2reg + const_prop
! at -O1+ sees the canary's stored -1 and the (separate) array
! alloca as non-aliasing and folds the canary read to literal
! -1, hiding the runtime corruption. Pass-by-ref makes the
! canary's address escape and forces a real memory load.
!
! XFAIL: audit5 CRITICAL-1 (i8 array store overflows neighbors)
! CHECK: -1
program audit5_c1_i8_array_store_overflow
  integer(kind=1) :: canary
  integer(kind=1) :: arr(3)
  canary = -1
  arr(1) = 10
  arr(2) = 20
  arr(3) = 30   ! 4-byte STR overflows past arr's end into canary
  call observe(canary)
contains
  subroutine observe(c)
    integer(kind=1) :: c
    print *, c
  end subroutine
end program
