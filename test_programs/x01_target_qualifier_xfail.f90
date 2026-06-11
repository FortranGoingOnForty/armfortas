! Sprint x01 fixture: an XFAIL qualified for a target this program does
! not fail on. The selector names the musl triple, which no sweep host
! runs until the x11 link story — so the qualifier is inactive
! everywhere the harness executes, the annotation behaves as if
! absent, and the program must pass — proving inactive qualifier ==
! no annotation. (Was x86_64-linux until x07 made that target real.)
! XFAIL(x86_64-linux-musl): inactive-qualifier semantics fixture; no real bug
! CHECK: 7
program x01_qualifier_xfail
  implicit none
  integer :: i
  i = 3 + 4
  print *, i
end program x01_qualifier_xfail
