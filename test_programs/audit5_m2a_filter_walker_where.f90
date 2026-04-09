! Audit #5 MAJOR-2 (a) — filtered USE ONLY references inside a
! WHERE construct slip past check_no_filtered_refs.
!
! The audit-4 MAJOR-1 walker only handles Assignment/Print/Write
! /Read/If/Do/Call/Block. WHERE is missing, so a `where (arr <
! hidden)` reference compiles cleanly and crashes at runtime
! because the filtered name resolves to const_int 0 and the
! mask logic does the wrong thing.
!
! Expected: compile-time error mentioning `hidden`.
!
! XFAIL: audit5 MAJOR-2 (filter walker doesn't cover WHERE)
! ERROR_EXPECTED: hidden
module audit5_m2a_mod
  integer :: visible = 1
  integer :: hidden = 999
end module audit5_m2a_mod

program audit5_m2a_filter_walker_where
  use audit5_m2a_mod, only: visible
  integer :: arr(3)
  arr = 0
  where (arr < hidden)
    arr = 1
  end where
  print *, arr
end program
