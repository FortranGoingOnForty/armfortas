! USE ONLY filtering: names not in the only-list must not be
! exposed. Before the fix, install_globals_as_locals installed
! every global regardless of USE statements.
! Audit CRITICAL-2.
!
! CHECK: 7
program use_only_filter
  use mod_filter, only: shown
  print *, shown
end program

module mod_filter
  integer :: shown = 7
  integer :: hidden = 99
end module mod_filter
