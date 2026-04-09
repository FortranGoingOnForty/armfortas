! Audit #6 probe — USE ONLY filter walker covers STOP code
! expressions. Stop is a transfer-of-control stmt that the
! original walker dropped entirely.
!
! ERROR_EXPECTED: hidden
module audit6_filter_stop_mod
  integer :: visible = 1
  integer :: hidden = 999
end module audit6_filter_stop_mod

program audit6_filter_stop_code
  use audit6_filter_stop_mod, only: visible
  stop hidden
end program
