! Pins the x86 bool-deref miscompile found during l02 (2026-06-10):
! `if (c)` on a LOGICAL dummy branches on the low byte of the POINTER
! instead of loading the pointed-to value — movzbl gets a register
! source where it needs a memory operand (isel/regalloc addressing
! form for i8 loads through a pointer vreg). IR verified correct;
! arm64 correct. Owned by x07's parity sweep; the XFAIL flips to a
! hard failure there the moment the fix lands.
! XFAIL(x86_64): bool loads through pointer vregs select a register-source movzx (x07)
! CHECK: false-branch
! CHECK: true-branch
program x07_bool_dummy_branch
  implicit none
  call mr(.false.)
  call mr(.true.)
contains
  subroutine mr(c)
    logical, intent(in) :: c
    if (c) then
      print *, 'true-branch'
    else
      print *, 'false-branch'
    end if
  end subroutine mr
end program x07_bool_dummy_branch
