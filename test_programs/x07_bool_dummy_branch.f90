! Pins the x86 bool-deref miscompile found during l02 and fixed in
! x07: `if (c)` on a LOGICAL dummy branched on the low byte of the
! POINTER — the extending loads shared an opcode with register
! zero-extension, so the allocator could not tell addresses from
! values. Movzx/Movsx now have RM (memory-source) variants carrying
! the MovRM address convention.
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
