! Two modules each declaring an identically-named variable. We
! bring them both in under disambiguating rename aliases, which
! is the standards-compliant way to use both at once.
! Audit CRITICAL-4 (the non-rename form triggers a diagnostic).
!
! CHECK: 1
! CHECK: 2
program two_module_rename
  use mod_aa, only: ax => val
  use mod_bb, only: bx => val
  print *, ax
  print *, bx
end program

module mod_aa
  integer :: val = 1
end module mod_aa

module mod_bb
  integer :: val = 2
end module mod_bb
