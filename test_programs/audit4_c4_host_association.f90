! Audit #4 CRITICAL-4 — contained subprograms inherit host USE
! imports via host association.
!
! Fixed: lower_unit now takes a `host_uses` slice. Each call
! computes a `combined_uses = host_uses ++ self_uses` and passes
! the combined list both to install_globals_as_locals AND to any
! nested contains recursion. Per F2018 §16.2, host association
! makes parent USE imports visible inside contained subprograms.
!
! While here, also fixed the latent bug that Subroutine/Function
! arms didn't recurse into their own `contains` at all — only
! Program did. fortsh has nested-subprogram chains where this
! would have silently dropped the inner subs.
!
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
