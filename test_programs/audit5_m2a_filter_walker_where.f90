! Audit #5 MAJOR-2 (a) — filtered USE ONLY references inside a
! WHERE construct.
!
! Fixed: check_filtered_in_stmt now handles WhereConstruct,
! WhereStmt, and the rest of the executable Stmt variants. The
! mask expression of `where (arr < hidden)` is walked, and a
! reference to a USE-ONLY-filtered name now produces a
! compile-time diagnostic instead of silently lowering to
! const_int 0.
!
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
