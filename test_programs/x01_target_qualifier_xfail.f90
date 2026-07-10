! Historical target-qualifier fixture. Inactive XFAIL selector behavior is now
! covered directly by run_programs harness unit tests, so this source remains
! an ordinary cross-target smoke test rather than carrying fictitious debt.
! CHECK: 7
program x01_qualifier_xfail
  implicit none
  integer :: i
  i = 3 + 4
  print *, i
end program x01_qualifier_xfail
