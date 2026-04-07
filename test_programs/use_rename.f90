! USE rename: `use m, only: local => remote` binds the module's
! `remote` to the current scope as `local`. Before the fix, the
! lowerer installed remote under its own name and the local
! alias fell through to const_int 0.
! Audit CRITICAL-3.
!
! CHECK: 42
! CHECK: 100
program use_rename
  use mod_rn, only: alpha => real_name, beta
  print *, alpha
  print *, beta
end program

module mod_rn
  integer :: real_name = 42
  integer :: beta = 100
end module mod_rn
