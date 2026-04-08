! Audit #4 CRITICAL-4 — contained subprograms don't inherit host
! USE imports (host association is broken).
!
! Per F2018 §16.2, names imported into a host program unit via
! USE are visible in its contained subprograms via host
! association. Our lower_unit lowers each contained subprogram
! independently, passing only its own (empty) `uses` slice to
! install_globals_as_locals. The parent's USE imports never make
! it down — `v` here lowers as an undefined name and falls
! through to const_int 0.
!
! This is a foundational scoping bug. fortsh's ~55 modules with
! contained subprograms reference host-imported names everywhere;
! every reference would silently return 0.
!
! Fix: thread the host's accumulated `uses` into each contained
! unit's install_globals_as_locals call.
!
! XFAIL: audit CRITICAL-4 (host association broken in contains)
! CHECK: 42
program audit4_c4_host_association
  use audit4_c4_mod
  call inner()
contains
  subroutine inner()
    print *, v   ! host association — should resolve to mod_v.v == 42
  end subroutine
end program

module audit4_c4_mod
  integer :: v = 42
end module audit4_c4_mod
