! Audit #4 MAJOR-1 — USE ONLY filtering allows explicit references
! to filtered names, silently lowering them to const_int 0.
!
! With `implicit none`, referencing a name that the only-list
! excluded must be a compile-time error per F2018 §11.2.2 (USE
! association brings in only the listed names; anything else is
! undefined). Without implicit none it should at minimum warn
! that an implicit local is being created from a host name.
!
! Today the C2 fix correctly excludes `hidden` from the locals
! map (so it's not USE-imported), but sema doesn't track the
! "could-have-been-imported" set, and the lowerer falls through
! to const_int 0 without diagnosing.
!
! Fix: track the module's public surface that the only-list
! filtered out, and have sema diagnose references against it.
!
! Both annotations: ERROR_EXPECTED for the post-fix behavior
! (we eventually diagnose), XFAIL for today (we don't).
!
! XFAIL: audit MAJOR-1 (USE ONLY hidden name not diagnosed)
! ERROR_EXPECTED: hidden
program audit4_maj1_use_only_hidden_ref
  use audit4_maj1_mod, only: shown
  implicit none
  print *, shown
  print *, hidden     ! must be diagnosed: not in only-list
end program

module audit4_maj1_mod
  integer :: shown = 7
  integer :: hidden = 99
end module audit4_maj1_mod
