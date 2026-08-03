! A branch from outside a structured construct must not enter its interior.
! Reject this at semantic validation instead of creating an unreachable,
! unterminated label block that fails IR verification.
!
! FLAGS: --std=f2023
! ERROR_EXPECTED: control transfer to label 10 enters a structured construct
program ar43_goto_into_block
  implicit none
  go to 10
  block
10  continue
  end block
end program ar43_goto_into_block
