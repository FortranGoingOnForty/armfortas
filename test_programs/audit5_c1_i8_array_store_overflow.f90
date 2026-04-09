! Audit #5 CRITICAL-1 — i8 array element stores now coerce the
! RHS to the element type before storing.
!
! Fixed: lower_array_store now calls coerce_to_type on the value
! before emitting `b.store(...)`. The CRITICAL-2 fix from audit
! #4 made isel pick the store opcode by the value's IR type, so
! the IR's value type is the source of truth for store width.
! The dual obligation is that array element assignments must
! truncate to the declared element type — and that's what the
! coerce_to_type call enforces.
!
! Test design: the canary is passed BY REFERENCE to a contained
! subroutine. mem2reg refuses to promote allocas whose address
! escapes, so the canary's value comes from a real memory load
! rather than a const-folded register. Without this, mem2reg
! at -O1+ would see the canary alloca and the array alloca as
! non-aliasing and hide the bug behind a const-prop'd literal.
!
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
