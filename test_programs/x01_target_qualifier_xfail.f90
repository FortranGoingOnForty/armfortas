! Sprint x01 fixture: an XFAIL qualified for a target this program does
! not fail on. On arm64-macos (and any non-linux host) the qualifier is
! inactive, the annotation behaves as if absent, and the program must
! pass — proving inactive qualifier == no annotation.
! XFAIL(x86_64-linux): placeholder for x07 triage; no real bug
! CHECK: 7
program x01_qualifier_xfail
  implicit none
  integer :: i
  i = 3 + 4
  print *, i
end program x01_qualifier_xfail
